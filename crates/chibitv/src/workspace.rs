use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Local, NaiveDateTime, TimeDelta};
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, DeliverySystem};
use crate::channel_scanner::{ChannelScanner, ScanRequest};
use crate::event_crawler::EventCrawler;
use crate::recorder::{Recorder, Recording};
use crate::scheduler::Scheduler;
use crate::service::{Service, ServiceKey};
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
    ServiceNotFound,
    TunerBusy,
    /// No tuner receives the broadcast the channel is on.
    NoTuner(DeliverySystem),
    StreamingUnavailable,
    /// No event crawler is configured, so the guide cannot be refreshed.
    EventCrawlerUnavailable,
    /// No channel scanner is configured, so nothing can be scanned for.
    ChannelScannerUnavailable,
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

pub struct Workspace {
    store: Arc<dyn Store>,
    /// The channels being served, which a saved scan replaces.
    channels: RwLock<Vec<Channel>>,
    streams: Option<Streams>,
    event_crawler: Option<Arc<EventCrawler>>,
    channel_scanner: Option<Arc<ChannelScanner>>,
    /// What the last scan found, kept for whoever asked for it to read back.
    scan_result: Arc<Mutex<Vec<NewChannel>>>,
    recorder: Option<Arc<Recorder>>,
    tasks: Arc<Tasks>,
    scheduler: Arc<Scheduler>,
}

impl Workspace {
    pub fn new(store: Arc<dyn Store>, channels: Vec<Channel>, streams: Option<Streams>) -> Self {
        let tasks = Arc::<Tasks>::default();

        Self {
            store,
            channels: RwLock::new(channels),
            streams,
            event_crawler: None,
            channel_scanner: None,
            scan_result: Arc::default(),
            recorder: None,
            scheduler: Scheduler::spawn(Arc::clone(&tasks)),
            tasks,
        }
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

    /// The database the channels, their services and the schedule are kept
    /// in.
    pub fn store(&self) -> &dyn Store {
        self.store.as_ref()
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

        self.tasks
            .spawn_blocking(
                TaskKind::RefreshEvents,
                "Refreshing the programme guide",
                move |task| crawler.crawl(&channels, dwell_time, task),
            )
            .map_err(|error| match error {
                SpawnError::AlreadyRunning => WorkspaceError::TaskAlreadyRunning,
            })
    }

    /// Starts looking for the channels on air in the background.
    ///
    /// What the scan finds replaces what the last one did, and is read back
    /// with [`Workspace::scan_result`]. The channels being served are left
    /// alone until the channels to keep of what was found are handed to
    /// [`Workspace::create_channels`].
    pub fn scan_channels(&self, request: ScanRequest) -> Result<Task, WorkspaceError> {
        let scanner = self
            .channel_scanner
            .clone()
            .ok_or(WorkspaceError::ChannelScannerUnavailable)?;
        request
            .validate()
            .map_err(WorkspaceError::ScanNotPossible)?;

        let result = Arc::clone(&self.scan_result);

        self.tasks
            .spawn_blocking(
                TaskKind::ScanChannels,
                "Scanning for channels",
                move |task| {
                    *result.lock().unwrap() = scanner.scan(&request, Some(task))?;

                    Ok(())
                },
            )
            .map_err(|error| match error {
                SpawnError::AlreadyRunning => WorkspaceError::TaskAlreadyRunning,
            })
    }

    /// What the last scan found, which is empty until one has finished.
    pub fn scan_result(&self) -> Vec<NewChannel> {
        self.scan_result.lock().unwrap().clone()
    }

    /// Keeps the channels given, and serves them from now on.
    ///
    /// This is what a scan comes to: whoever asked for one hands back the
    /// channels of it worth keeping, edited or picked over first if they like.
    /// Nothing has to be restarted, as the service catalogs that come along
    /// are stored with them and the channels served are swapped for the ones
    /// now stored.
    pub async fn create_channels(
        &self,
        channels: &[NewChannel],
    ) -> Result<Vec<Channel>, WorkspaceError> {
        self.store
            .create_channels(channels)
            .await
            .map_err(WorkspaceError::Internal)?;

        let stored = self
            .store
            .load_channels()
            .await
            .map_err(WorkspaceError::Internal)?;

        let channels = stored.iter().map(Channel::from).collect::<Vec<_>>();
        *self.channels.write().unwrap() = channels.clone();

        Ok(channels)
    }

    /// Books a recording of the event, which starts shortly before the
    /// programme does and stops once it is over.
    pub async fn schedule_recording(
        &self,
        key: ServiceKey,
        event_id: u16,
    ) -> Result<Task, WorkspaceError> {
        let recorder = self
            .recorder
            .clone()
            .ok_or(WorkspaceError::RecordingUnavailable)?;
        let (service, channel) = self
            .service(key)
            .await?
            .ok_or(WorkspaceError::ServiceNotFound)?;
        let event = self
            .store
            .find_event(key, event_id)
            .await
            .map_err(WorkspaceError::Internal)?
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

    /// The services of the channels being served, each with the channel
    /// carrying it, in the order of their keys.
    ///
    /// The signalling of a network describes every stream of it, and a service
    /// of one no channel being served carries cannot be watched or recorded,
    /// so it is left out.
    pub async fn services(&self) -> Result<Vec<(Service, Channel)>, WorkspaceError> {
        let services = self
            .store
            .find_services()
            .await
            .map_err(WorkspaceError::Internal)?;

        Ok(services
            .into_iter()
            .filter_map(|service| {
                let channel = self.channel_of(service.key)?;
                Some((service, channel))
            })
            .collect())
    }

    /// The service of the key with the channel carrying it, when one being
    /// served does.
    pub async fn service(
        &self,
        key: ServiceKey,
    ) -> Result<Option<(Service, Channel)>, WorkspaceError> {
        let Some(channel) = self.channel_of(key) else {
            return Ok(None);
        };
        let service = self
            .store
            .find_service(key)
            .await
            .map_err(WorkspaceError::Internal)?;

        Ok(service.map(|service| (service, channel)))
    }

    /// The physical channel the service of the key is carried on, which is
    /// the one carrying its stream.
    ///
    /// Two channels carrying the same stream — a relay station on another
    /// frequency — share its services, which go with the first of them.
    pub fn channel_of(&self, key: ServiceKey) -> Option<Channel> {
        self.channels
            .read()
            .unwrap()
            .iter()
            .find(|channel| channel.stream_id == Some(key.stream_id))
            .cloned()
    }

    /// Attaches to the shared stream of the service, tuning to it first when
    /// nobody is streaming it yet.
    pub async fn subscribe_stream(
        &self,
        key: ServiceKey,
    ) -> Result<StreamSubscription, WorkspaceError> {
        let (_, channel) = self
            .service(key)
            .await?
            .ok_or(WorkspaceError::ServiceNotFound)?;

        let streams = self
            .streams
            .as_ref()
            .ok_or(WorkspaceError::StreamingUnavailable)?;

        let stream = streams
            .subscribe(key, &channel)
            .await
            .map_err(|error| match error {
                SubscribeError::TunerBusy => WorkspaceError::TunerBusy,
                SubscribeError::NoTuner(system) => WorkspaceError::NoTuner(system),
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

    async fn store() -> Arc<dyn Store> {
        crate::store::open("sqlite::memory:").await.unwrap()
    }

    fn channel() -> Channel {
        Channel {
            id: 0,
            name: "UHF 20".to_string(),
            inner: ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
            stream_id: Some(SERVICE.stream_id),
        }
    }

    #[tokio::test]
    async fn subscribing_an_unknown_service_fails() {
        let workspace = Workspace::new(store().await, vec![channel()], None);

        let result = workspace.subscribe_stream(SERVICE).await;

        assert!(matches!(result, Err(WorkspaceError::ServiceNotFound)));
    }

    #[tokio::test]
    async fn refreshing_events_without_a_crawler_fails() {
        let workspace = Workspace::new(store().await, vec![channel()], None);

        let result = workspace.refresh_events(Duration::from_secs(1));

        assert!(matches!(
            result,
            Err(WorkspaceError::EventCrawlerUnavailable)
        ));
    }

    #[tokio::test]
    async fn recording_without_a_configured_storage_fails() {
        let workspace = Workspace::new(store().await, vec![channel()], None);

        let result = workspace.schedule_recording(SERVICE, 1).await;

        assert!(matches!(result, Err(WorkspaceError::RecordingUnavailable)));
    }

    #[tokio::test]
    async fn serves_the_channels_it_is_told_to_keep() {
        let store = store().await;
        let workspace = Workspace::new(Arc::clone(&store), vec![], None);

        let Ok(channels) = workspace
            .create_channels(&[NewChannel {
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
            }])
            .await
        else {
            panic!("the channels could not be kept");
        };

        assert_eq!(channels.len(), 1);
        assert_eq!(workspace.channels().len(), 1);
        assert_eq!(store.load_channels().await.unwrap().len(), 1);

        // The services that came along are there to be watched without the
        // server having been restarted.
        let Ok(Some((_, channel))) = workspace.service(SERVICE).await else {
            panic!("the service is not served");
        };
        assert_eq!(channel.id, channels[0].id);
    }

    fn channel_carrying(id: usize, stream_id: u16) -> Channel {
        Channel {
            id,
            stream_id: Some(stream_id),
            ..channel()
        }
    }

    async fn store_with_services(streams: &[u16]) -> Arc<dyn Store> {
        let store = store().await;
        for &stream_id in streams {
            store
                .save_services(
                    stream_id,
                    &[crate::store::StoredService {
                        id: 101,
                        name: format!("Service of {stream_id}"),
                        provider_name: String::new(),
                    }],
                )
                .await
                .unwrap();
        }

        store
    }

    /// The signalling of a network describes every stream of it, including the
    /// ones no channel is served from.
    #[tokio::test]
    async fn leaves_out_the_services_no_channel_carries() {
        let workspace = Workspace::new(
            store_with_services(&[100, 200]).await,
            vec![channel_carrying(1, 100)],
            None,
        );

        let Ok(services) = workspace.services().await else {
            panic!("the services could not be read");
        };

        assert_eq!(
            services
                .iter()
                .map(|(service, channel)| (service.key.stream_id, channel.id))
                .collect::<Vec<_>>(),
            [(100, 1)]
        );
        assert!(matches!(
            workspace
                .service(ServiceKey {
                    stream_id: 200,
                    service_id: 101,
                })
                .await,
            Ok(None)
        ));
    }

    #[tokio::test]
    async fn serves_a_stream_carried_twice_from_the_first_channel() {
        let workspace = Workspace::new(
            store_with_services(&[100]).await,
            vec![channel_carrying(1, 100), channel_carrying(2, 100)],
            None,
        );

        let Ok(services) = workspace.services().await else {
            panic!("the services could not be read");
        };

        assert_eq!(services.len(), 1);
        assert_eq!(services[0].1.id, 1);
    }

    #[tokio::test]
    async fn subscribing_without_configured_streams_fails() {
        let workspace = Workspace::new(store().await, vec![], None);
        let Ok(_) = workspace
            .create_channels(&[NewChannel {
                name: "UHF 20".to_string(),
                inner: channel().inner,
                transport_stream_id: Some(SERVICE.stream_id),
                services: vec![crate::store::StoredService {
                    id: SERVICE.service_id,
                    name: "Channel".to_string(),
                    provider_name: String::new(),
                }],
            }])
            .await
        else {
            panic!("the channels could not be kept");
        };

        let result = workspace.subscribe_stream(SERVICE).await;

        assert!(matches!(result, Err(WorkspaceError::StreamingUnavailable)));
    }
}
