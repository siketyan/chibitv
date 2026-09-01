use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Local, NaiveDateTime, TimeDelta};
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::{Channel, ChannelInner};
use crate::event_crawler::EventCrawler;
use crate::recorder::{Recorder, Recording};
use crate::registry::{Registry, Service};
use crate::scheduler::Scheduler;
use crate::service_information::Signal;
use crate::stream::{Stream, Streams, SubscribeError};
use crate::task::{CancelError, SpawnError, Task, TaskId, TaskKind, Tasks};

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
    /// No storage is configured, so nothing can be recorded.
    RecordingUnavailable,
    EventNotFound,
    /// The event is not announced with a time to record it at.
    EventNotScheduled,
    /// The event is over, so recording it is no longer possible.
    EventPassed,
    TaskNotFound,
    TaskNotCancellable,
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
    registry: Arc<Registry>,
    channels: Vec<Channel>,
    streams: Option<Streams>,
    event_crawler: Option<Arc<EventCrawler>>,
    recorder: Option<Arc<Recorder>>,
    tasks: Arc<Tasks>,
    scheduler: Arc<Scheduler>,
}

impl Workspace {
    pub fn new(registry: Arc<Registry>, channels: Vec<Channel>, streams: Option<Streams>) -> Self {
        let tasks = Arc::<Tasks>::default();

        Self {
            registry,
            channels,
            streams,
            event_crawler: None,
            recorder: None,
            scheduler: Scheduler::spawn(Arc::clone(&tasks)),
            tasks,
        }
    }

    pub fn with_event_crawler(mut self, crawler: EventCrawler) -> Self {
        self.event_crawler = Some(Arc::new(crawler));
        self
    }

    pub fn with_recorder(mut self, recorder: Recorder) -> Self {
        self.recorder = Some(Arc::new(recorder));
        self
    }

    pub fn channels(&self) -> impl Iterator<Item = (usize, &Channel)> {
        self.channels.iter().enumerate()
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
        let channels = self.channels.clone();
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

    /// Books a recording of the event, which starts shortly before the
    /// programme does and stops once it is over.
    pub fn schedule_recording(
        &self,
        service_id: u16,
        event_id: u16,
    ) -> Result<Task, WorkspaceError> {
        let recorder = self
            .recorder
            .clone()
            .ok_or(WorkspaceError::RecordingUnavailable)?;
        let service = self
            .registry
            .get_service_by_id(service_id)
            .ok_or(WorkspaceError::ServiceNotFound)?;
        let channel = self
            .channel_of(&service)
            .ok_or(WorkspaceError::ChannelNotFound)?
            .clone();
        let event = self
            .registry
            .get_event_by_id(service_id, event_id)
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
            service_id,
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

    /// The physical channel the service is carried on.
    fn channel_of(&self, service: &Service) -> Option<&Channel> {
        self.channels.iter().find(|channel| match &channel.inner {
            ChannelInner::IsdbS { stream_id, .. } => {
                *stream_id == u32::from(service.transport_stream_id)
            }
            ChannelInner::IsdbT { .. }
            | ChannelInner::BonIsdbS { .. }
            | ChannelInner::BonIsdbT { .. } => service.channel_id == channel.id,
        })
    }

    /// Attaches to the shared stream of the service, tuning to it first when
    /// nobody is streaming it yet.
    pub async fn subscribe_stream(
        &self,
        service_id: u16,
    ) -> Result<StreamSubscription, WorkspaceError> {
        let service = self
            .registry
            .get_service_by_id(service_id)
            .ok_or(WorkspaceError::ServiceNotFound)?;

        let channel = self
            .channel_of(&service)
            .ok_or(WorkspaceError::ChannelNotFound)?;

        let streams = self
            .streams
            .as_ref()
            .ok_or(WorkspaceError::StreamingUnavailable)?;

        let stream = streams
            .subscribe(service_id, channel)
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

        let result = workspace.subscribe_stream(0x5678).await;

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

        let result = workspace.schedule_recording(0x5678, 1);

        assert!(matches!(result, Err(WorkspaceError::RecordingUnavailable)));
    }

    #[tokio::test]
    async fn subscribing_without_configured_streams_fails() {
        let registry = Arc::new(Registry::default());
        registry.put_cached_service(0, 0x1234, 0x5678, "Channel".to_string(), String::new());
        let workspace = Workspace::new(registry, vec![channel()], None);

        let result = workspace.subscribe_stream(0x5678).await;

        assert!(matches!(result, Err(WorkspaceError::StreamingUnavailable)));
    }
}
