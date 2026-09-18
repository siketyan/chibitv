use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Local, NaiveDateTime, TimeDelta};
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, DeliverySystem};
use crate::channel_scanner::{ChannelScanner, ScanRequest};
use crate::event_crawler::EventCrawler;
use crate::recorder::{Recorder, Recording};
use crate::registry::{Registry, Service, ServiceKey};
use crate::scheduler::Scheduler;
use crate::service_information::Signal;
use crate::store::{NewChannel, Store};
use crate::stream::{Stream, Streams, SubscribeError};
use crate::task::{CancelError, DeleteError, SpawnError, Task, TaskId, TaskKind, Tasks};

/// How long before a programme starts its recording does, so that a broadcast
/// running early is still recorded from its beginning.
const RECORDING_LEAD: TimeDelta = TimeDelta::seconds(15);

/// How long a recording keeps going after the programme is due to end, for a
/// broadcast running late.
const RECORDING_MARGIN: TimeDelta = TimeDelta::seconds(30);

pub enum WorkspaceError {
    ChannelNotFound,
    ServiceNotFound,
    TunerBusy,
    StreamingUnavailable,
    /// No event crawler is configured, so the guide cannot be refreshed.
    EventCrawlerUnavailable,
    /// No channel scanner is configured, so nothing can be scanned for.
    ChannelScannerUnavailable,
    /// No database is configured, so the channels cannot be written.
    ChannelStoreUnavailable,
    /// No scan has finished, so there is nothing of one to keep.
    ScanResultUnavailable,
    /// The scan was asked for something it cannot walk.
    ScanNotPossible(anyhow::Error),
    /// No storage is configured, so nothing can be recorded.
    RecordingUnavailable,
    EventNotFound,
    /// The event is not announced with a time to record it at.
    EventNotScheduled,
    /// The event is over, so recording it is no longer possible.
    EventPassed,
    TaskNotFound,
    TaskNotCancellable,
    /// The task is still to run, or running, so it cannot be forgotten yet.
    TaskNotFinished,
    /// A task doing the same work is already running.
    TaskAlreadyRunning,
    Internal(anyhow::Error),
}

pub struct StreamSubscription {
    pub stream: Arc<Stream>,
    pub init_segment: Option<Bytes>,
    pub fmp4: BroadcastStream<Bytes>,
    pub signals: BroadcastStream<Signal>,
}

/// What a scan found, and the broadcast it walked.
///
/// Which broadcast it was is what keeping the result replaces the channels of,
/// so it is remembered alongside them: a terrestrial scan says nothing about
/// the satellite channels and leaves them be.
#[derive(Clone, Debug)]
struct ScanOutcome {
    delivery_system: DeliverySystem,
    channels: Vec<NewChannel>,
}

pub struct Workspace {
    registry: Arc<Registry>,
    /// The channels being served, which a saved scan replaces.
    channels: RwLock<Vec<Channel>>,
    store: Option<Arc<dyn Store>>,
    streams: Option<Streams>,
    event_crawler: Option<Arc<EventCrawler>>,
    channel_scanner: Option<Arc<ChannelScanner>>,
    /// What the last scan found, kept for whoever asked for it to read back.
    scan_result: Arc<Mutex<Option<ScanOutcome>>>,
    recorder: Option<Arc<Recorder>>,
    tasks: Arc<Tasks>,
    scheduler: Arc<Scheduler>,
}

impl Workspace {
    pub fn new(registry: Arc<Registry>, channels: Vec<Channel>, streams: Option<Streams>) -> Self {
        let tasks = Arc::<Tasks>::default();

        Self {
            registry,
            channels: RwLock::new(channels),
            store: None,
            streams,
            event_crawler: None,
            channel_scanner: None,
            scan_result: Arc::default(),
            recorder: None,
            scheduler: Scheduler::spawn(Arc::clone(&tasks)),
            tasks,
        }
    }

    /// Keeps the channels in the database, which is what a saved scan writes
    /// them to.
    pub fn with_channel_store(mut self, store: Arc<dyn Store>) -> Self {
        self.store = Some(store);
        self
    }

    pub fn with_event_crawler(mut self, crawler: EventCrawler) -> Self {
        self.event_crawler = Some(Arc::new(crawler));
        self
    }

    pub fn with_channel_scanner(mut self, scanner: ChannelScanner) -> Self {
        self.channel_scanner = Some(Arc::new(scanner));
        self
    }

    pub fn with_recorder(mut self, recorder: Recorder) -> Self {
        self.recorder = Some(Arc::new(recorder));
        self
    }

    /// The channels being served, in the order the database keeps them.
    pub fn channels(&self) -> Vec<Channel> {
        self.channels.read().unwrap().clone()
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn tasks(&self) -> &Arc<Tasks> {
        &self.tasks
    }

    /// Starts collecting the programme guide in the background, spending
    /// `dwell_time` on each configured channel.
    pub fn refresh_events(&self, dwell_time: Duration) -> Result<Task, WorkspaceError> {
        let crawler = self
            .event_crawler
            .clone()
            .ok_or(WorkspaceError::EventCrawlerUnavailable)?;
        let channels = self.channels();
        let registry = Arc::clone(&self.registry);

        self.tasks
            .spawn_blocking(
                TaskKind::RefreshEvents,
                "Refreshing the programme guide",
                move |task| crawler.crawl(&channels, registry, dwell_time, task),
            )
            .map_err(|error| match error {
                SpawnError::AlreadyRunning => WorkspaceError::TaskAlreadyRunning,
            })
    }

    /// Starts looking for the channels on air in the background.
    ///
    /// What the scan finds replaces what the last one did, and is read back
    /// with [`Workspace::scan_result`]. The channels being served are left
    /// alone until [`Workspace::save_scan_result`] is asked to keep what was
    /// found.
    pub fn scan_channels(&self, request: ScanRequest) -> Result<Task, WorkspaceError> {
        let scanner = self
            .channel_scanner
            .clone()
            .ok_or(WorkspaceError::ChannelScannerUnavailable)?;
        request
            .validate()
            .map_err(WorkspaceError::ScanNotPossible)?;

        let result = Arc::clone(&self.scan_result);
        let delivery_system = request.delivery_system.into();

        self.tasks
            .spawn_blocking(
                TaskKind::ScanChannels,
                "Scanning for channels",
                move |task| {
                    let channels = scanner.scan(&request, Some(task))?;
                    *result.lock().unwrap() = Some(ScanOutcome {
                        delivery_system,
                        channels,
                    });

                    Ok(())
                },
            )
            .map_err(|error| match error {
                SpawnError::AlreadyRunning => WorkspaceError::TaskAlreadyRunning,
            })
    }

    /// What the last scan found, which is empty until one has finished.
    pub fn scan_result(&self) -> Vec<NewChannel> {
        self.scan_result
            .lock()
            .unwrap()
            .as_ref()
            .map(|outcome| outcome.channels.clone())
            .unwrap_or_default()
    }

    /// Keeps what the last scan found, and serves it from now on.
    ///
    /// The channels of the broadcast the scan walked are replaced by what it
    /// found, so one that has left the air stops being served, while the
    /// channels of the other broadcasts are left alone. Nothing has to be
    /// restarted: the registry is seeded with the service catalogs that were
    /// found, and the channels served are swapped for the ones now stored.
    pub async fn save_scan_result(&self) -> Result<Vec<Channel>, WorkspaceError> {
        let store = self
            .store
            .clone()
            .ok_or(WorkspaceError::ChannelStoreUnavailable)?;
        let outcome = self
            .scan_result
            .lock()
            .unwrap()
            .clone()
            .ok_or(WorkspaceError::ScanResultUnavailable)?;

        store
            .replace_channels(outcome.delivery_system, &outcome.channels)
            .await
            .map_err(WorkspaceError::Internal)?;

        let stored = store
            .load_channels()
            .await
            .map_err(WorkspaceError::Internal)?;
        self.registry.put_channels(&stored);

        let channels = stored.iter().map(Channel::from).collect::<Vec<_>>();
        *self.channels.write().unwrap() = channels.clone();

        Ok(channels)
    }

    /// Hands the workspace a scan outcome, for a test that has no tuner to
    /// run a scan with.
    #[cfg(test)]
    fn put_scan_result(&self, delivery_system: DeliverySystem, channels: Vec<NewChannel>) {
        *self.scan_result.lock().unwrap() = Some(ScanOutcome {
            delivery_system,
            channels,
        });
    }

    /// Books a recording of the event, which starts shortly before the
    /// programme does and stops once it is over.
    pub fn schedule_recording(
        &self,
        key: ServiceKey,
        event_id: u16,
    ) -> Result<Task, WorkspaceError> {
        let recorder = self
            .recorder
            .clone()
            .ok_or(WorkspaceError::RecordingUnavailable)?;
        let service = self
            .registry
            .get_service(key)
            .ok_or(WorkspaceError::ServiceNotFound)?;
        let channel = self
            .channel_of(&service)
            .ok_or(WorkspaceError::ChannelNotFound)?;
        let event = self
            .registry
            .get_event(key, event_id)
            .ok_or(WorkspaceError::EventNotFound)?;

        let (Some(start_time), Some(duration)) = (event.start_time, event.duration) else {
            return Err(WorkspaceError::EventNotScheduled);
        };
        let starts_at = broadcast_time(start_time)?;
        let ends_at = broadcast_time(start_time + duration)? + RECORDING_MARGIN;
        if ends_at <= Local::now() {
            return Err(WorkspaceError::EventPassed);
        }

        // The programme names the recording, falling back to the service for
        // one announced without a title.
        let title = event.name.unwrap_or(service.name);
        let recording = Recording {
            channel,
            service_id: key.service_id,
            title: title.clone(),
            starts_at,
            ends_at,
        };

        // A programme already on air is recorded from now on, rather than from
        // a start time that has been and gone.
        let at = (starts_at - RECORDING_LEAD).max(Local::now());

        Ok(self.scheduler.schedule(
            TaskKind::Record,
            format!("Recording {title}"),
            at,
            move |task| recorder.record(&recording, task),
        ))
    }

    pub fn cancel_task(&self, id: TaskId) -> Result<Task, WorkspaceError> {
        self.tasks.cancel(id).map_err(|error| match error {
            CancelError::NotFound => WorkspaceError::TaskNotFound,
            CancelError::NotCancellable => WorkspaceError::TaskNotCancellable,
        })
    }

    /// Forgets a task that is over, so that it stops being listed.
    pub fn delete_task(&self, id: TaskId) -> Result<(), WorkspaceError> {
        self.tasks.delete(id).map_err(|error| match error {
            DeleteError::NotFound => WorkspaceError::TaskNotFound,
            DeleteError::NotFinished => WorkspaceError::TaskNotFinished,
        })
    }

    /// The physical channel the service of the key is carried on, when the
    /// registry has come across that service.
    pub fn channel_of_key(&self, key: ServiceKey) -> Option<Channel> {
        self.channel_of(&self.registry.get_service(key)?)
    }

    /// The physical channel the service is carried on.
    ///
    /// The registry only keeps a service under the channel carrying its
    /// stream, so the identifier it holds is the one to look for.
    fn channel_of(&self, service: &Service) -> Option<Channel> {
        self.channels
            .read()
            .unwrap()
            .iter()
            .find(|channel| channel.id == service.channel_id)
            .cloned()
    }

    /// Attaches to the shared stream of the service, tuning to it first when
    /// nobody is streaming it yet.
    pub async fn subscribe_stream(
        &self,
        key: ServiceKey,
    ) -> Result<StreamSubscription, WorkspaceError> {
        let service = self
            .registry
            .get_service(key)
            .ok_or(WorkspaceError::ServiceNotFound)?;

        let channel = self
            .channel_of(&service)
            .ok_or(WorkspaceError::ChannelNotFound)?;

        let streams = self
            .streams
            .as_ref()
            .ok_or(WorkspaceError::StreamingUnavailable)?;

        let stream = streams
            .subscribe(key, &channel)
            .await
            .map_err(|error| match error {
                SubscribeError::TunerBusy => WorkspaceError::TunerBusy,
                SubscribeError::Internal(error) => WorkspaceError::Internal(error),
            })?;

        let (init_segment, fmp4) = stream.subscribe_fmp4();
        let signals = stream.subscribe_signal();

        Ok(StreamSubscription {
            stream,
            init_segment,
            fmp4: BroadcastStream::new(fmp4),
            signals: BroadcastStream::new(signals),
        })
    }
}

/// Reads a time announced by the broadcast as one of the server clock, which
/// runs on the zone the broadcast schedules against.
fn broadcast_time(value: NaiveDateTime) -> Result<DateTime<Local>, WorkspaceError> {
    value.and_local_timezone(Local).earliest().ok_or_else(|| {
        WorkspaceError::Internal(anyhow::anyhow!("the broadcast time does not exist"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::ChannelInner;

    const SERVICE: ServiceKey = ServiceKey {
        stream_id: 0x1234,
        service_id: 0x5678,
    };

    fn channel() -> Channel {
        Channel {
            id: 0,
            name: "UHF 20".to_string(),
            inner: ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
        }
    }

    #[tokio::test]
    async fn subscribing_an_unknown_service_fails() {
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None);

        let result = workspace.subscribe_stream(SERVICE).await;

        assert!(matches!(result, Err(WorkspaceError::ServiceNotFound)));
    }

    #[tokio::test]
    async fn refreshing_events_without_a_crawler_fails() {
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None);

        let result = workspace.refresh_events(Duration::from_secs(1));

        assert!(matches!(
            result,
            Err(WorkspaceError::EventCrawlerUnavailable)
        ));
    }

    #[tokio::test]
    async fn recording_without_a_configured_storage_fails() {
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None);

        let result = workspace.schedule_recording(SERVICE, 1);

        assert!(matches!(result, Err(WorkspaceError::RecordingUnavailable)));
    }

    #[tokio::test]
    async fn keeping_a_scan_that_never_ran_fails() {
        let store = crate::store::open("sqlite::memory:").await.unwrap();
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None)
            .with_channel_store(store);

        let result = workspace.save_scan_result().await;

        assert!(matches!(result, Err(WorkspaceError::ScanResultUnavailable)));
    }

    #[tokio::test]
    async fn keeping_a_scan_without_a_database_fails() {
        let workspace = Workspace::new(Arc::new(Registry::default()), vec![channel()], None);
        workspace.put_scan_result(DeliverySystem::IsdbT, vec![]);

        let result = workspace.save_scan_result().await;

        assert!(matches!(
            result,
            Err(WorkspaceError::ChannelStoreUnavailable)
        ));
    }

    #[tokio::test]
    async fn serves_the_channels_a_kept_scan_found() {
        let store = crate::store::open("sqlite::memory:").await.unwrap();
        let registry = Arc::new(Registry::default());
        let workspace = Workspace::new(Arc::clone(&registry), vec![], None)
            .with_channel_store(Arc::clone(&store));
        workspace.put_scan_result(
            DeliverySystem::IsdbT,
            vec![NewChannel {
                name: "UHF 20".to_string(),
                inner: ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
                transport_stream_id: Some(SERVICE.stream_id),
                services: vec![crate::store::StoredService {
                    id: SERVICE.service_id,
                    name: "Service".to_string(),
                    provider_name: "Provider".to_string(),
                }],
            }],
        );

        let Ok(channels) = workspace.save_scan_result().await else {
            panic!("the scan result could not be kept");
        };

        assert_eq!(channels.len(), 1);
        assert_eq!(workspace.channels().len(), 1);
        assert_eq!(store.load_channels().await.unwrap().len(), 1);

        // The services it found are there to be watched without the server
        // having been restarted.
        let service = registry.get_service(SERVICE).unwrap();
        assert_eq!(service.channel_id, channels[0].id);
    }

    #[tokio::test]
    async fn subscribing_without_configured_streams_fails() {
        let registry = Arc::new(Registry::default());
        registry.put_cached_service(0, SERVICE, "Channel".to_string(), String::new());
        let workspace = Workspace::new(registry, vec![channel()], None);

        let result = workspace.subscribe_stream(SERVICE).await;

        assert!(matches!(result, Err(WorkspaceError::StreamingUnavailable)));
    }
}
