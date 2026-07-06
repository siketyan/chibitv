use std::collections::BTreeMap;
use std::io::{BufReader, BufWriter};
use std::ops::DerefMut;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;

use bytes::Bytes;
use mpeg2ts::ts::TsPacketWriter;
use tokio::sync::broadcast::{Receiver, Sender, channel};
use tracing::info;

use crate::channel::Channel;
use crate::descrambler::Descrambler;
use crate::m2ts::M2tsMuxer;
use crate::mmt::MmtDemuxer;
use crate::registry::Registry;
use crate::remux::{Remux, Remuxer, Signal};
use crate::tuner::Tuner;

const READ_BUFFER_SIZE: usize = 188 * 8192;

struct ChannelWriter(Sender<Bytes>);

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.send(Bytes::copy_from_slice(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // do nothing
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
    registry: Arc<Registry>,
    tuner: Arc<dyn Tuner>,
    descrambler: Arc<Mutex<Descrambler>>,
    state: Arc<RwLock<StreamState>>,
    tx: Sender<Bytes>,
    signal_tx: Sender<Signal>,
    rx: Receiver<Bytes>,
}

impl Stream {
    pub fn open(
        registry: Arc<Registry>,
        tuner: Arc<dyn Tuner>,
        descrambler: Descrambler,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = channel::<Bytes>(1024 * 1024);
        let (signal_tx, mut signal_rx) = channel::<Signal>(1);
        let state = Arc::new(RwLock::new(StreamState::default()));

        {
            let state = Arc::clone(&state);

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
        }

        Ok(Self {
            registry,
            tuner,
            descrambler: Arc::new(Mutex::new(descrambler)),
            state,
            tx,
            signal_tx,
            rx,
        })
    }

    fn start_remuxer(&self) -> anyhow::Result<()> {
        let reader = BufReader::with_capacity(READ_BUFFER_SIZE, self.tuner.open()?);
        let demux = MmtDemuxer::new(reader, Arc::clone(&self.descrambler));
        let writer = BufWriter::new(ChannelWriter(self.tx.clone()));
        let mux = M2tsMuxer::new(TsPacketWriter::new(writer));
        let mut remuxer = Remuxer::new(
            demux,
            mux,
            Some(self.signal_tx.clone()),
            Some(Arc::clone(&self.registry)),
        );

        let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();
        let handle = std::thread::spawn(move || remuxer.run(Some(kill_rx)));

        self.state.write().unwrap().handle = Some((handle, kill_tx));

        Ok(())
    }

    pub fn subscribe(&self) -> Receiver<Bytes> {
        self.rx.resubscribe()
    }

    pub fn set_channel(&self, service_id: u16, channel: &Channel) -> anyhow::Result<()> {
        let state = std::mem::take(self.state.write().unwrap().deref_mut());

        if let Some((handle, kill_tx)) = state.handle {
            // Kill the current session.
            let _ = kill_tx.send(());
            handle.join().unwrap()?;
        };

        // Tune to the channel.
        self.tuner.tune(channel.clone())?;

        self.start_remuxer()?;

        let mut state = self.state.write().unwrap();
        state.service_id = Some(service_id);
        state.event_id = None;

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
