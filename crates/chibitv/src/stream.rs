use std::collections::BTreeMap;
use std::io::{BufReader, Cursor};
use std::ops::DerefMut;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;

use bytes::{Bytes, BytesMut};
use mpeg2ts::ts::{TsPacket, TsPacketWriter, WriteTsPacket};
use tokio::sync::broadcast::{Receiver, Sender, channel};
use tracing::info;

use crate::channel::Channel;
use crate::descrambler::Descrambler;
use crate::m2ts::M2tsMuxer;
use crate::mmt::MmtDemuxer;
use crate::registry::Registry;
use crate::remux::{Remux, Remuxer, Signal};
use crate::tuner::Tuner;

struct ChannelWriter(Sender<Bytes>);

impl WriteTsPacket for ChannelWriter {
    fn write_ts_packet(&mut self, packet: &TsPacket) -> mpeg2ts::Result<()> {
        let mut buf = BytesMut::zeroed(TsPacket::SIZE);
        let mut writer = TsPacketWriter::new(Cursor::new(buf.as_mut()));
        writer.write_ts_packet(packet)?;
        self.0.send(buf.freeze()).ok();
        Ok(())
    }
}

#[derive(Default)]
pub struct StreamState {
    handle: Option<(
        JoinHandle<anyhow::Result<()>>,
        tokio::sync::oneshot::Sender<()>,
    )>,
    service_id: Option<u16>,
    event_id: Option<u16>,
}

pub struct Stream {
    tuner: Arc<dyn Tuner>,
    remuxer: Arc<Mutex<Box<dyn Remux>>>,
    state: Arc<RwLock<StreamState>>,
    rx: Receiver<Bytes>,
    signal_rx: Receiver<Signal>,
}

impl Stream {
    pub fn open(
        registry: Arc<Registry>,
        tuner: Arc<dyn Tuner>,
        descrambler: Descrambler,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = channel::<Bytes>(1024 * 1024);
        let (signal_tx, signal_rx) = channel::<Signal>(1);

        let reader = BufReader::new(tuner.open()?);
        let demux = MmtDemuxer::new(reader, descrambler);
        let mux = M2tsMuxer::new(ChannelWriter(tx));
        let remuxer = Remuxer::new(demux, mux, signal_tx, registry);

        Ok(Self {
            tuner,
            remuxer: Arc::new(Mutex::new(Box::new(remuxer))),
            state: Arc::new(RwLock::new(StreamState::default())),
            rx,
            signal_rx,
        })
    }

    pub fn run(&self) {
        let remuxer = self.remuxer.clone();
        let state = self.state.clone();
        let mut signal_rx = self.signal_rx.resubscribe();

        tokio::spawn(async move {
            loop {
                let Ok(signal) = signal_rx.recv().await else {
                    continue;
                };

                match signal {
                    Signal::EventChanged { event_id, .. } => {
                        info!(event_id, "Event changed");
                        state.write().unwrap().event_id = Some(event_id);
                    }
                }
            }
        });

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();
        let handle = std::thread::spawn(move || remuxer.lock().unwrap().run(kill_rx));

        self.state.write().unwrap().handle = Some((handle, kill_tx));
    }

    pub fn subscribe(&self) -> Receiver<Bytes> {
        self.rx.resubscribe()
    }

    pub fn set_channel(&self, service_id: u16, channel: &Channel) -> anyhow::Result<()> {
        let state = std::mem::take(self.state.write().unwrap().deref_mut());

        if let Some((handle, kill_tx)) = state.handle {
            // Kill the current session.
            kill_tx.send(()).unwrap();
            handle.join().unwrap()?;

            // Clear the remuxer state.
            self.remuxer.lock().unwrap().clear();

            // Restart a new session.
            self.run();
        };

        // Tune to the channel.
        self.tuner.tune(channel.clone())?;
        self.state.write().unwrap().service_id = Some(service_id);

        Ok(())
    }

    pub fn get_service_id(&self) -> Option<u16> {
        self.state.read().unwrap().service_id
    }

    pub fn get_event_id(&self) -> Option<u16> {
        self.state.read().unwrap().event_id
    }
}

#[derive(Default)]
pub struct Streams {
    streams: BTreeMap<u32, Mutex<Stream>>,
}

impl Streams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_stream(&self, stream_id: u32) -> Option<MutexGuard<'_, Stream>> {
        self.streams.get(&stream_id)?.lock().ok()
    }

    pub fn add_stream(&mut self, stream_id: u32, stream: Stream) {
        self.streams.insert(stream_id, Mutex::new(stream));
    }
}
