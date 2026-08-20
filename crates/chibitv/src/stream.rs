use std::collections::HashMap;
use std::io::BufReader;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::broadcast::{Receiver, Sender, channel as broadcast_channel};
use tracing::info;

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;

use crate::cas::PcscCasModule;
use crate::channel::{Channel, ChannelInner};
use crate::demux::Demux;
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::mp4::{FragmentedMp4Muxer, WriteMp4Fragment};
use crate::registry::Registry;
use crate::remux::Remuxer;
use crate::service_information::{ServiceInformationProcessor, Signal};
use crate::store::EventWriter;
use crate::tuner::{AcquireError, TunerLease, Tuners};

const READ_BUFFER_SIZE: usize = 188 * 8192;
const BROADCAST_CAPACITY: usize = 8192;

/// How long a subscriber keeps waiting for a tuner to become free.
///
/// A stream that just lost its last subscriber releases its tuner
/// asynchronously (the remuxer thread has to notice the kill signal first), so
/// a channel switch briefly sees every tuner in use.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
const ACQUIRE_RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// What a remuxer thread is tuned to.
#[derive(Clone, Copy)]
struct StreamTarget {
    channel_id: usize,
    /// The service to follow, or `None` while the whole transport stream is
    /// streamed and no single service is being watched.
    service_id: Option<u16>,
}

pub enum SubscribeError {
    TunerBusy,
    Internal(anyhow::Error),
}

struct Fmp4StreamWriter {
    tx: Sender<Bytes>,
    init_segment: Arc<Mutex<Option<Bytes>>>,
}

impl WriteMp4Fragment for Fmp4StreamWriter {
    fn write_fragment(&mut self, data: Bytes) -> anyhow::Result<()> {
        let mut init_segment = self.init_segment.lock().unwrap();
        if init_segment.is_none() {
            *init_segment = Some(data.clone());
        }

        let _ = self.tx.send(data);
        Ok(())
    }
}

/// A single tuned service, shared by every client streaming it.
///
/// The tuner stays occupied as long as at least one `Arc` of the stream is
/// alive; dropping the last one signals the remuxer thread to stop, which
/// closes the tuner device and releases the lease.
pub struct Stream {
    service_id: u16,
    event_id: Arc<RwLock<Option<u16>>>,
    fmp4_tx: Sender<Bytes>,
    fmp4_init_segment: Arc<Mutex<Option<Bytes>>>,
    signal_tx: Sender<Signal>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Stream {
    pub fn service_id(&self) -> u16 {
        self.service_id
    }

    pub fn event_id(&self) -> Option<u16> {
        *self.event_id.read().unwrap()
    }

    pub fn subscribe_fmp4(&self) -> (Option<Bytes>, Receiver<Bytes>) {
        let init_segment = self.fmp4_init_segment.lock().unwrap();
        let rx = self.fmp4_tx.subscribe();
        info!(
            service_id = self.service_id,
            receivers = self.fmp4_tx.receiver_count(),
            "fMP4 stream client subscribed"
        );
        (init_segment.clone(), rx)
    }

    pub fn subscribe_signal(&self) -> Receiver<Signal> {
        self.signal_tx.subscribe()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(kill_tx) = self.kill_tx.take() {
            let _ = kill_tx.send(());
        }

        info!(service_id = self.service_id, "Stream stopped");
    }
}

/// Starts and shares [`Stream`]s, one per requested service.
/// Where a stream hands the service information it demultiplexes.
#[derive(Clone)]
struct ServiceInformationSink {
    registry: Arc<Registry>,
    /// Where the schedule is kept between runs.
    events: EventWriter,
}

pub struct Streams {
    registry: Arc<Registry>,
    tuners: Arc<Tuners>,
    cas: Arc<PcscCasModule>,
    b61_descrambler: Option<Descrambler>,
    events: EventWriter,
    streams: tokio::sync::Mutex<HashMap<u16, Weak<Stream>>>,
}

impl Streams {
    pub fn new(
        registry: Arc<Registry>,
        tuners: Arc<Tuners>,
        cas: Arc<PcscCasModule>,
        b61_descrambler: Option<Descrambler>,
        events: EventWriter,
    ) -> Self {
        Self {
            registry,
            tuners,
            cas,
            b61_descrambler,
            events,
            streams: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the running stream for the service, starting one on a free
    /// tuner when nobody is streaming it yet.
    pub async fn subscribe(
        &self,
        service_id: u16,
        channel: &Channel,
    ) -> Result<Arc<Stream>, SubscribeError> {
        let deadline = tokio::time::Instant::now() + ACQUIRE_TIMEOUT;

        loop {
            let mut streams = self.streams.lock().await;
            if let Some(stream) = streams.get(&service_id).and_then(Weak::upgrade) {
                return Ok(stream);
            }

            // Tuning is blocking device I/O, so it runs off the async runtime.
            // The streams lock is held across it on purpose: concurrent
            // requests for the same service must wait and share the stream
            // instead of racing for another tuner.
            let starter = self.stream_starter(service_id, channel);
            let result = tokio::task::spawn_blocking(starter)
                .await
                .map_err(|error| SubscribeError::Internal(error.into()))?;

            match result {
                Ok(stream) => {
                    streams.retain(|_, stream| stream.strong_count() > 0);
                    streams.insert(service_id, Arc::downgrade(&stream));
                    return Ok(stream);
                }
                Err(SubscribeError::TunerBusy) if tokio::time::Instant::now() < deadline => {}
                Err(error) => return Err(error),
            }

            drop(streams);
            tokio::time::sleep(ACQUIRE_RETRY_INTERVAL).await;
        }
    }

    fn stream_starter(
        &self,
        service_id: u16,
        channel: &Channel,
    ) -> impl FnOnce() -> Result<Arc<Stream>, SubscribeError> + Send + 'static {
        let service_information = ServiceInformationSink {
            registry: Arc::clone(&self.registry),
            events: self.events.clone(),
        };
        let tuners = Arc::clone(&self.tuners);
        let cas = Arc::clone(&self.cas);
        let b61_descrambler = self.b61_descrambler.clone();
        let channel = channel.clone();

        move || {
            let tuner = tuners.try_acquire().map_err(|error| match error {
                AcquireError::Busy => SubscribeError::TunerBusy,
                AcquireError::NotConfigured => SubscribeError::Internal(error.into()),
            })?;
            info!(tuner_id = tuner.id(), service_id, "Acquired tuner");

            start_stream(
                service_information,
                cas,
                b61_descrambler,
                tuner,
                service_id,
                &channel,
            )
            .map_err(SubscribeError::Internal)
        }
    }
}

fn start_stream(
    service_information: ServiceInformationSink,
    cas: Arc<PcscCasModule>,
    b61_descrambler: Option<Descrambler>,
    tuner: TunerLease,
    service_id: u16,
    channel: &Channel,
) -> anyhow::Result<Arc<Stream>> {
    tuner.tune(channel.clone())?;
    let reader = tuner.open()?;

    let (fmp4_tx, _) = broadcast_channel::<Bytes>(BROADCAST_CAPACITY);
    let fmp4_init_segment = Arc::new(Mutex::new(None));
    let (signal_tx, _) = broadcast_channel::<Signal>(16);
    let event_id = Arc::new(RwLock::new(None));

    let kill_tx = match &channel.inner {
        ChannelInner::IsdbS { .. } => {
            let descrambler = b61_descrambler
                .ok_or_else(|| anyhow::anyhow!("B61 descrambler is not configured"))?;
            let reader = BufReader::with_capacity(READ_BUFFER_SIZE, reader);
            spawn_remuxer(
                MmtDemuxer::new(reader, descrambler),
                StreamTarget {
                    channel_id: channel.id,
                    service_id: Some(service_id),
                },
                service_information,
                &fmp4_tx,
                &fmp4_init_segment,
                &signal_tx,
                &event_id,
            )
        }
        ChannelInner::IsdbT { .. } => {
            let descrambler = B25Descrambler::init(cas)?;
            // A service of zero streams the whole transport stream instead of
            // picking one service out of it.
            let target_service_id = (service_id != 0).then_some(service_id);
            let demux = match target_service_id {
                Some(service_id) => M2tsDemuxer::new_for_service(reader, descrambler, service_id),
                None => M2tsDemuxer::new(reader, descrambler),
            };
            spawn_remuxer(
                demux,
                StreamTarget {
                    channel_id: channel.id,
                    service_id: target_service_id,
                },
                service_information,
                &fmp4_tx,
                &fmp4_init_segment,
                &signal_tx,
                &event_id,
            )
        }
    }?;

    info!(service_id, channel = %channel.name, "Stream started");

    Ok(Arc::new(Stream {
        service_id,
        event_id,
        fmp4_tx,
        fmp4_init_segment,
        signal_tx,
        kill_tx: Some(kill_tx),
    }))
}

fn spawn_remuxer<D>(
    demux: D,
    target: StreamTarget,
    service_information: ServiceInformationSink,
    fmp4_tx: &Sender<Bytes>,
    fmp4_init_segment: &Arc<Mutex<Option<Bytes>>>,
    signal_tx: &Sender<Signal>,
    event_id: &Arc<RwLock<Option<u16>>>,
) -> anyhow::Result<tokio::sync::oneshot::Sender<()>>
where
    D: Demux + Send + 'static,
{
    let fmp4_writer = Fmp4StreamWriter {
        tx: fmp4_tx.clone(),
        init_segment: Arc::clone(fmp4_init_segment),
    };
    let mux = FragmentedMp4Muxer::new(fmp4_writer);
    let mut remuxer = Remuxer::new(demux, mux)?;
    let mut processor = ServiceInformationProcessor::new(
        target.channel_id,
        Some(service_information.registry),
        Some(signal_tx.clone()),
    )
    .watching_service(target.service_id)
    .storing_events(service_information.events);

    let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel();
    let event_id = Arc::clone(event_id);
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            loop {
                if kill_rx.try_recv().is_ok() {
                    break;
                }

                let Some(signaling) = remuxer.next()? else {
                    break;
                };
                processor.process(signaling)?;
                *event_id.write().unwrap() = processor.current_event_id();
            }

            remuxer.finish()
        })();

        if let Err(error) = result {
            tracing::error!(channel_id = target.channel_id, %error, "Stream remuxer failed");
        }
    });

    Ok(kill_tx)
}
