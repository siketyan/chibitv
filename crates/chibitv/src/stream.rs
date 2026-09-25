use std::collections::HashMap;
use std::io::BufReader;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::broadcast::{Receiver, Sender, channel as broadcast_channel};
use tracing::info;

use chibitv_b25::B25Descrambler;
use chibitv_b61::Descrambler;

use crate::cas::SharedCasModule;
use crate::channel::{Channel, ChannelInner, DeliverySystem};
use crate::demux::{Demux, DescramblingRefusal, descrambling_refusal};
use crate::m2ts::M2tsDemuxer;
use crate::mmt::MmtDemuxer;
use crate::mp4::{FragmentedMp4Muxer, WriteMp4Fragment};
use crate::remux::Remuxer;
use crate::service::ServiceKey;
use crate::service_information::{ServiceInformationProcessor, ServiceInformationWriter, Signal};
use crate::tuner::{AcquireError, TunerInput, Tuners};

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
    /// No tuner receives the broadcast the channel is on.
    NoTuner(DeliverySystem),
    Internal(anyhow::Error),
}

/// Why a stream stopped, as far as it is worth telling a viewer apart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StreamFailureKind {
    /// No contract on the card covers the programme being watched.
    NotContracted,
    /// The card handed over no key to descramble the programme with, for some
    /// other reason.
    DescramblingRefused,
    /// Anything else that stopped the pipeline.
    Internal,
}

/// What stopped a stream, on its way to whoever is watching it.
#[derive(Clone, Debug)]
pub struct StreamFailure {
    pub kind: StreamFailureKind,
    /// What went wrong, as the error put it.
    pub message: String,
}

impl StreamFailure {
    fn of(error: &anyhow::Error) -> Self {
        let kind = match descrambling_refusal(error) {
            Some(DescramblingRefusal::NotContracted) => StreamFailureKind::NotContracted,
            Some(DescramblingRefusal::Other) => StreamFailureKind::DescramblingRefused,
            None => StreamFailureKind::Internal,
        };

        Self {
            kind,
            message: error.to_string(),
        }
    }
}

/// Where a stream leaves what stopped it.
///
/// A stream stops for good — the card answers the next ECM the way it answered
/// this one — so what stopped it is kept beside the channel it is announced
/// on, and a client attaching afterwards is told as readily as the ones that
/// were already watching.
#[derive(Clone)]
struct StreamFailures {
    last: Arc<Mutex<Option<StreamFailure>>>,
    tx: Sender<StreamFailure>,
}

impl StreamFailures {
    fn new() -> Self {
        let (tx, _) = broadcast_channel(1);
        Self {
            last: Arc::new(Mutex::new(None)),
            tx,
        }
    }

    /// Announcing under the lock is what keeps a client from being told twice,
    /// or not at all: whoever subscribes either reads the failure below or
    /// receives it, never both.
    fn record(&self, failure: StreamFailure) {
        let mut last = self.last.lock().unwrap();
        if last.is_some() {
            return;
        }

        *last = Some(failure.clone());
        let _ = self.tx.send(failure);
    }

    fn subscribe(&self) -> (Option<StreamFailure>, Receiver<StreamFailure>) {
        let last = self.last.lock().unwrap();
        let rx = self.tx.subscribe();
        (last.clone(), rx)
    }
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

/// What a remuxer thread writes and the clients of a [`Stream`] read.
#[derive(Clone)]
struct StreamOutputs {
    fmp4_tx: Sender<Bytes>,
    fmp4_init_segment: Arc<Mutex<Option<Bytes>>>,
    signal_tx: Sender<Signal>,
    failures: StreamFailures,
    event_id: Arc<RwLock<Option<u16>>>,
}

impl StreamOutputs {
    fn new() -> Self {
        let (fmp4_tx, _) = broadcast_channel::<Bytes>(BROADCAST_CAPACITY);
        let (signal_tx, _) = broadcast_channel::<Signal>(16);

        Self {
            fmp4_tx,
            fmp4_init_segment: Arc::new(Mutex::new(None)),
            signal_tx,
            failures: StreamFailures::new(),
            event_id: Arc::new(RwLock::new(None)),
        }
    }
}

/// A single tuned service, shared by every client streaming it.
///
/// The tuner stays occupied as long as at least one `Arc` of the stream is
/// alive; dropping the last one signals the remuxer thread to stop, which
/// gives the tuner back to tunelithd.
pub struct Stream {
    key: ServiceKey,
    outputs: StreamOutputs,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Stream {
    pub fn key(&self) -> ServiceKey {
        self.key
    }

    pub fn event_id(&self) -> Option<u16> {
        *self.outputs.event_id.read().unwrap()
    }

    pub fn subscribe_fmp4(&self) -> (Option<Bytes>, Receiver<Bytes>) {
        let init_segment = self.outputs.fmp4_init_segment.lock().unwrap();
        let rx = self.outputs.fmp4_tx.subscribe();
        info!(
            service_id = self.key.service_id,
            receivers = self.outputs.fmp4_tx.receiver_count(),
            "fMP4 stream client subscribed"
        );
        (init_segment.clone(), rx)
    }

    pub fn subscribe_signal(&self) -> Receiver<Signal> {
        self.outputs.signal_tx.subscribe()
    }

    /// What stopped the stream, if it already has, and whatever stops it next.
    pub fn subscribe_failure(&self) -> (Option<StreamFailure>, Receiver<StreamFailure>) {
        self.outputs.failures.subscribe()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(kill_tx) = self.kill_tx.take() {
            let _ = kill_tx.send(());
        }

        info!(service_id = self.key.service_id, "Stream stopped");
    }
}

/// Starts and shares [`Stream`]s, one per requested service.
pub struct Streams {
    writer: ServiceInformationWriter,
    tuners: Arc<Tuners>,
    cas: Arc<SharedCasModule>,
    cas_master_key: [u8; 32],
    streams: tokio::sync::Mutex<HashMap<ServiceKey, Weak<Stream>>>,
}

impl Streams {
    pub fn new(
        writer: ServiceInformationWriter,
        tuners: Arc<Tuners>,
        cas: Arc<SharedCasModule>,
        cas_master_key: [u8; 32],
    ) -> Self {
        Self {
            writer,
            tuners,
            cas,
            cas_master_key,
            streams: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Returns the running stream for the service, starting one on a free
    /// tuner when nobody is streaming it yet.
    pub async fn subscribe(
        &self,
        key: ServiceKey,
        channel: &Channel,
    ) -> Result<Arc<Stream>, SubscribeError> {
        let deadline = tokio::time::Instant::now() + ACQUIRE_TIMEOUT;

        loop {
            let mut streams = self.streams.lock().await;
            if let Some(stream) = streams.get(&key).and_then(Weak::upgrade) {
                return Ok(stream);
            }

            // Tuning is blocking device I/O, so it runs off the async runtime.
            // The streams lock is held across it on purpose: concurrent
            // requests for the same service must wait and share the stream
            // instead of racing for another tuner.
            let starter = self.stream_starter(key, channel);
            let result = tokio::task::spawn_blocking(starter)
                .await
                .map_err(|error| SubscribeError::Internal(error.into()))?;

            match result {
                Ok(stream) => {
                    streams.retain(|_, stream| stream.strong_count() > 0);
                    streams.insert(key, Arc::downgrade(&stream));
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
        key: ServiceKey,
        channel: &Channel,
    ) -> impl FnOnce() -> Result<Arc<Stream>, SubscribeError> + Send + 'static {
        let writer = self.writer.clone();
        let tuners = Arc::clone(&self.tuners);
        let cas = Arc::clone(&self.cas);
        let cas_master_key = self.cas_master_key;
        let channel = channel.clone();

        move || {
            let reader = tuners.tune(&channel).map_err(|error| match error {
                AcquireError::Busy => SubscribeError::TunerBusy,
                AcquireError::Unsupported(system) => SubscribeError::NoTuner(system),
                AcquireError::Failed(error) => SubscribeError::Internal(error),
            })?;
            info!(
                tuner = reader.tuner(),
                service_id = key.service_id,
                "Acquired tuner"
            );

            start_stream(writer, cas, cas_master_key, reader, key, &channel)
                .map_err(SubscribeError::Internal)
        }
    }
}

fn start_stream(
    writer: ServiceInformationWriter,
    cas: Arc<SharedCasModule>,
    cas_master_key: [u8; 32],
    reader: TunerInput,
    key: ServiceKey,
    channel: &Channel,
) -> anyhow::Result<Arc<Stream>> {
    let outputs = StreamOutputs::new();

    let kill_tx = match &channel.inner {
        ChannelInner::IsdbS3 { .. } => {
            let descrambler = Descrambler::init(cas, cas_master_key, true)?;
            let reader = BufReader::with_capacity(READ_BUFFER_SIZE, reader);
            spawn_remuxer(
                MmtDemuxer::new(reader, descrambler),
                StreamTarget {
                    channel_id: channel.id,
                    service_id: Some(key.service_id),
                },
                writer,
                &outputs,
            )
        }
        ChannelInner::IsdbT { .. } | ChannelInner::IsdbS { .. } => {
            let descrambler = B25Descrambler::init(cas, true)?;
            // A service of zero streams the whole transport stream instead of
            // picking one service out of it.
            let target_service_id = (key.service_id != 0).then_some(key.service_id);
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
                writer,
                &outputs,
            )
        }
    }?;

    info!(service_id = key.service_id, channel = %channel.name, "Stream started");

    Ok(Arc::new(Stream {
        key,
        outputs,
        kill_tx: Some(kill_tx),
    }))
}

fn spawn_remuxer<D>(
    demux: D,
    target: StreamTarget,
    writer: ServiceInformationWriter,
    outputs: &StreamOutputs,
) -> anyhow::Result<tokio::sync::oneshot::Sender<()>>
where
    D: Demux + Send + 'static,
{
    let fmp4_writer = Fmp4StreamWriter {
        tx: outputs.fmp4_tx.clone(),
        init_segment: Arc::clone(&outputs.fmp4_init_segment),
    };
    let mux = FragmentedMp4Muxer::new(fmp4_writer);
    let mut remuxer = Remuxer::new(demux, mux)?;
    let mut processor =
        ServiceInformationProcessor::new(Some(writer), Some(outputs.signal_tx.clone()))
            .watching_service(target.service_id);

    let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel();
    let failures = outputs.failures.clone();
    let event_id = Arc::clone(&outputs.event_id);
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
            // Nothing is coming out of this stream any more, so whoever is
            // watching it is told why rather than left with a frozen picture.
            failures.record(StreamFailure::of(&error));
        }
    });

    Ok(kill_tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_programme_no_contract_covers_from_anything_else_that_stops_a_stream() {
        assert_eq!(
            StreamFailure::of(
                &chibitv_b25::EcmRefusedError {
                    return_code: 0x8901
                }
                .into()
            )
            .kind,
            StreamFailureKind::NotContracted
        );
        assert_eq!(
            StreamFailure::of(
                &chibitv_b61::EcmRefusedError {
                    return_code: 0xA101
                }
                .into()
            )
            .kind,
            StreamFailureKind::DescramblingRefused
        );
        assert_eq!(
            StreamFailure::of(&anyhow::anyhow!("the tuner went away")).kind,
            StreamFailureKind::Internal
        );
    }

    #[tokio::test]
    async fn tells_a_client_what_stopped_the_stream_whichever_side_of_it_it_arrived() {
        let failures = StreamFailures::new();

        let (before, mut watching) = failures.subscribe();
        assert!(before.is_none());

        failures.record(StreamFailure {
            kind: StreamFailureKind::NotContracted,
            message: "no contract".to_string(),
        });

        assert_eq!(
            watching.recv().await.unwrap().kind,
            StreamFailureKind::NotContracted
        );

        // One that attaches afterwards reads it instead of waiting for a
        // second announcement that is never coming.
        let (after, _) = failures.subscribe();
        assert_eq!(after.unwrap().kind, StreamFailureKind::NotContracted);
    }
}
