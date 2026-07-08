use std::collections::BTreeMap;
use std::io::BufReader;
use std::ops::DerefMut;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::JoinHandle;

use bytes::Bytes;
use tokio::sync::broadcast::{Receiver, Sender, channel};
use tracing::info;

use chibitv_b61::Descrambler;

use crate::channel::Channel;
use crate::mmt::MmtDemuxer;
use crate::mp4::{FragmentedMp4Muxer, WriteMp4Fragment};
use crate::registry::Registry;
use crate::remux::{Remux, Remuxer, Signal};
use crate::tuner::Tuner;

const READ_BUFFER_SIZE: usize = 188 * 8192;
const BROADCAST_CAPACITY: usize = 8192;

type RemuxerHandle = (
    JoinHandle<anyhow::Result<()>>,
    tokio::sync::oneshot::Sender<()>,
);

struct Fmp4ChannelWriter {
    tx: Sender<Bytes>,
    init_segment: Arc<Mutex<Option<Bytes>>>,
}

impl WriteMp4Fragment for Fmp4ChannelWriter {
    fn write_fragment(&mut self, data: Bytes) -> anyhow::Result<()> {
        let mut init_segment = self.init_segment.lock().unwrap();
        if init_segment.is_none() {
            *init_segment = Some(data.clone());
        }

        let _ = self.tx.send(data);
        Ok(())
    }
}

#[derive(Default)]
pub struct StreamState {
    handle: Option<RemuxerHandle>,
    service_id: Option<u16>,
    event_id: Option<u16>,
}

pub struct Stream {
    registry: Arc<Registry>,
    tuner: Arc<dyn Tuner>,
    descrambler: Descrambler,
    state: Arc<RwLock<StreamState>>,
    fmp4_tx: Sender<Bytes>,
    fmp4_init_segment: Arc<Mutex<Option<Bytes>>>,
    signal_tx: Sender<Signal>,
}

impl Stream {
    pub fn open(
        registry: Arc<Registry>,
        tuner: Arc<dyn Tuner>,
        descrambler: Descrambler,
    ) -> anyhow::Result<Self> {
        let (fmp4_tx, _) = channel::<Bytes>(BROADCAST_CAPACITY);
        let fmp4_init_segment = Arc::new(Mutex::new(None));
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
            descrambler,
            state,
            fmp4_tx,
            fmp4_init_segment,
            signal_tx,
        })
    }

    fn start_remuxer(&self) -> anyhow::Result<()> {
        let reader = BufReader::with_capacity(READ_BUFFER_SIZE, self.tuner.open()?);
        let demux = MmtDemuxer::new(reader, self.descrambler.clone());
        let fmp4_writer = Fmp4ChannelWriter {
            tx: self.fmp4_tx.clone(),
            init_segment: Arc::clone(&self.fmp4_init_segment),
        };
        let mux = FragmentedMp4Muxer::new(fmp4_writer);
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

    pub fn subscribe_fmp4(&self) -> (Option<Bytes>, Receiver<Bytes>) {
        let init_segment = self.fmp4_init_segment.lock().unwrap();
        let rx = self.fmp4_tx.subscribe();
        info!(
            receivers = self.fmp4_tx.receiver_count(),
            "fMP4 stream client subscribed"
        );
        (init_segment.clone(), rx)
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

        *self.fmp4_init_segment.lock().unwrap() = None;
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
