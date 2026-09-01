use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};
use connectrpc::{
    ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::channel::ChannelInner;
use crate::proto::chibitv::v1::*;
use crate::registry;
use crate::service_information::Signal;
use crate::task;
use crate::workspace::{StreamSubscription, Workspace, WorkspaceError};

pub struct ChibitvServiceImpl {
    workspace: Arc<Workspace>,
}

impl ChibitvServiceImpl {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Self { workspace }
    }

    pub fn register(self, router: Router) -> Router {
        Arc::new(self).register(router)
    }
}

#[allow(refining_impl_trait)]
impl ChibitvService for ChibitvServiceImpl {
    async fn list_channels(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListChannelsRequest>,
    ) -> ServiceResult<ListChannelsResponse> {
        let channels = self
            .workspace
            .channels()
            .map(|(id, channel)| Channel {
                id: id as u32,
                name: channel.name.to_string(),
                delivery_system: delivery_system(&channel.inner).into(),
                ..Default::default()
            })
            .collect();

        Response::ok(ListChannelsResponse {
            channels,
            ..Default::default()
        })
    }

    async fn list_services(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListServicesRequest>,
    ) -> ServiceResult<ListServicesResponse> {
        let mut services = self.workspace.registry().get_all_services();
        services.sort_by_key(|service| service.id);
        let services = services.iter().map(Service::from).collect();

        Response::ok(ListServicesResponse {
            services,
            ..Default::default()
        })
    }

    async fn list_events(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ListEventsRequest>,
    ) -> ServiceResult<ListEventsResponse> {
        let mut events = if let Some(service_id) = request.service_id {
            let service_id = u16::try_from(service_id)
                .map_err(|_| ConnectError::invalid_argument("service_id is out of range"))?;
            self.workspace
                .registry()
                .get_events_by_service_id(service_id)
                .into_iter()
                .map(|event| (service_id, event))
                .collect::<Vec<_>>()
        } else {
            self.workspace
                .registry()
                .get_all_services()
                .into_iter()
                .flat_map(|service| {
                    self.workspace
                        .registry()
                        .get_events_by_service_id(service.id)
                        .into_iter()
                        .map(move |event| (service.id, event))
                })
                .collect::<Vec<_>>()
        };
        events.sort_by_key(|(service_id, event)| (*service_id, event.start_time, event.id));
        let events = events
            .iter()
            .map(|(service_id, event)| event_message(*service_id, event))
            .collect();

        Response::ok(ListEventsResponse {
            events,
            ..Default::default()
        })
    }

    async fn refresh_events(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, RefreshEventsRequest>,
    ) -> ServiceResult<RefreshEventsResponse> {
        const DEFAULT_DWELL_TIME_SECONDS: u32 = 10;
        const MAX_DWELL_TIME_SECONDS: u32 = 60;

        let dwell_time_seconds = match request.dwell_time_seconds {
            0 => DEFAULT_DWELL_TIME_SECONDS,
            seconds if seconds <= MAX_DWELL_TIME_SECONDS => seconds,
            _ => {
                return Err(ConnectError::invalid_argument(
                    "dwell_time_seconds must be at most 60",
                ));
            }
        };

        let task = self
            .workspace
            .refresh_events(Duration::from_secs(u64::from(dwell_time_seconds)))
            .map_err(workspace_error)?;

        Response::ok(RefreshEventsResponse {
            task: Some(task_message(&task)).into(),
            ..Default::default()
        })
    }

    async fn list_tasks(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListTasksRequest>,
    ) -> ServiceResult<ListTasksResponse> {
        Response::ok(ListTasksResponse {
            tasks: self
                .workspace
                .tasks()
                .list()
                .iter()
                .map(task_message)
                .collect(),
            ..Default::default()
        })
    }

    async fn watch_tasks(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, WatchTasksRequest>,
    ) -> ServiceResult<ServiceStream<Task>> {
        let tasks = self.workspace.tasks();
        // The updates are subscribed to before the tasks are listed, so that a
        // task changing in between is reported rather than missed. A client
        // keeps the tasks by their identifier, so seeing one twice is harmless.
        let updates = BroadcastStream::new(tasks.subscribe());
        let current = tokio_stream::iter(tasks.list());

        Response::stream_ok(
            current
                .chain(updates.filter_map(|update| update.ok()))
                .map(|task| Ok(task_message(&task))),
        )
    }

    async fn cancel_task(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, CancelTaskRequest>,
    ) -> ServiceResult<CancelTaskResponse> {
        let task = self
            .workspace
            .cancel_task(request.task_id)
            .map_err(workspace_error)?;

        Response::ok(CancelTaskResponse {
            task: Some(task_message(&task)).into(),
            ..Default::default()
        })
    }

    async fn schedule_recording(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ScheduleRecordingRequest>,
    ) -> ServiceResult<ScheduleRecordingResponse> {
        let service_id = u16::try_from(request.service_id)
            .map_err(|_| ConnectError::invalid_argument("service_id is out of range"))?;
        let event_id = u16::try_from(request.event_id)
            .map_err(|_| ConnectError::invalid_argument("event_id is out of range"))?;

        let task = self
            .workspace
            .schedule_recording(service_id, event_id)
            .map_err(workspace_error)?;

        Response::ok(ScheduleRecordingResponse {
            task: Some(task_message(&task)).into(),
            ..Default::default()
        })
    }

    async fn stream(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, StreamRequest>,
    ) -> ServiceResult<ServiceStream<StreamResponse>> {
        let service_id = u16::try_from(request.service_id)
            .map_err(|_| ConnectError::invalid_argument("service_id is out of range"))?;

        let StreamSubscription {
            stream,
            init_segment,
            fmp4,
            signals,
        } = self
            .workspace
            .subscribe_stream(service_id)
            .await
            .map_err(workspace_error)?;

        let initial_state = tokio_stream::iter([stream_state(&self.workspace, &stream, None)]);
        let init_segment = tokio_stream::iter(init_segment.into_iter().map(fmp4_response));
        let fmp4 = fmp4.filter_map(|data| data.ok().map(fmp4_response));
        let states = {
            let workspace = Arc::clone(&self.workspace);
            let stream = Arc::clone(&stream);
            signals.filter_map(move |signal| match signal.ok()? {
                Signal::EventChanged { event_id } => {
                    Some(stream_state(&workspace, &stream, Some(event_id)))
                }
            })
        };

        // The stream keeps the tuner occupied, so it is moved into the
        // response stream to release the tuner once every client is gone.
        Response::stream_ok(
            initial_state
                .chain(init_segment.chain(fmp4).merge(states))
                .map(move |response| {
                    let _stream = &stream;
                    Ok(response)
                }),
        )
    }
}

fn task_message(value: &task::Task) -> Task {
    Task {
        id: value.id,
        kind: task_kind(value.kind).into(),
        state: task_state(value.state).into(),
        title: value.title.clone(),
        message: value.message.clone(),
        progress: value.progress,
        cancellable: value.cancellable,
        error: value.error.clone().unwrap_or_default(),
        created_at: Some(DateTime::from(value.created_at)).into(),
        scheduled_at: value.scheduled_at.map(DateTime::from).into(),
        started_at: value.started_at.map(DateTime::from).into(),
        finished_at: value.finished_at.map(DateTime::from).into(),
        ..Default::default()
    }
}

fn task_kind(value: task::TaskKind) -> TaskKind {
    match value {
        task::TaskKind::RefreshEvents => TaskKind::RefreshEvents,
        task::TaskKind::Record => TaskKind::Record,
    }
}

fn task_state(value: task::TaskState) -> TaskState {
    match value {
        task::TaskState::Scheduled => TaskState::Scheduled,
        task::TaskState::Pending => TaskState::Pending,
        task::TaskState::Running => TaskState::Running,
        task::TaskState::Succeeded => TaskState::Succeeded,
        task::TaskState::Failed => TaskState::Failed,
        task::TaskState::Cancelled => TaskState::Cancelled,
    }
}

fn stream_state(
    workspace: &Workspace,
    stream: &crate::stream::Stream,
    event_id: Option<u16>,
) -> StreamResponse {
    let service_id = stream.service_id();
    let service = workspace.registry().get_service_by_id(service_id);
    let event = event_id
        .or_else(|| stream.event_id())
        .and_then(|event_id| workspace.registry().get_event_by_id(service_id, event_id));

    StreamResponse {
        payload: Some(stream_response::Payload::State(Box::new(StreamState {
            service: service.as_ref().map(Service::from).into(),
            event: service
                .as_ref()
                .zip(event.as_ref())
                .map(|(service, event)| event_message(service.id, event))
                .into(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

fn fmp4_response(data: bytes::Bytes) -> StreamResponse {
    StreamResponse {
        payload: Some(stream_response::Payload::Fmp4(data.to_vec())),
        ..Default::default()
    }
}

fn delivery_system(inner: &ChannelInner) -> DeliverySystem {
    match inner {
        ChannelInner::IsdbT { .. } | ChannelInner::BonIsdbT { .. } => DeliverySystem::IsdbT,
        ChannelInner::IsdbS { .. } | ChannelInner::BonIsdbS { .. } => DeliverySystem::IsdbS,
    }
}

fn workspace_error(error: WorkspaceError) -> ConnectError {
    match error {
        WorkspaceError::ChannelNotFound => ConnectError::not_found("channel not found"),
        WorkspaceError::ServiceNotFound => ConnectError::not_found("service not found"),
        WorkspaceError::TunerBusy => ConnectError::resource_exhausted("all tuners are in use"),
        WorkspaceError::StreamingUnavailable => {
            ConnectError::unavailable("streaming is unavailable")
        }
        WorkspaceError::EventCrawlerUnavailable => {
            ConnectError::failed_precondition("event crawler is unavailable")
        }
        WorkspaceError::RecordingUnavailable => {
            ConnectError::failed_precondition("recording is unavailable")
        }
        WorkspaceError::EventNotFound => ConnectError::not_found("event not found"),
        WorkspaceError::EventNotScheduled => {
            ConnectError::failed_precondition("the event is not announced with a time")
        }
        WorkspaceError::EventPassed => {
            ConnectError::failed_precondition("the event is over already")
        }
        WorkspaceError::TaskNotFound => ConnectError::not_found("task not found"),
        WorkspaceError::TaskNotCancellable => {
            ConnectError::failed_precondition("this task cannot be cancelled")
        }
        WorkspaceError::TaskAlreadyRunning => {
            ConnectError::already_exists("the same task is already running")
        }
        WorkspaceError::Internal(error) => {
            tracing::error!(?error, "Failed to open stream");
            ConnectError::internal("failed to open stream")
        }
    }
}

impl From<&registry::Service> for Service {
    fn from(value: &registry::Service) -> Self {
        Self {
            id: value.id.into(),
            name: value.name.clone(),
            provider_name: value.provider_name.clone(),
            channel_id: value.channel_id as u32,
            ..Default::default()
        }
    }
}

fn event_message(service_id: u16, value: &registry::Event) -> Event {
    Event {
        id: value.id.into(),
        title: value.name.clone().unwrap_or_default(),
        // The summary leads the detailed description, as the two describe the
        // event at different lengths rather than repeating each other.
        description: value
            .text
            .iter()
            .map(|text| (String::new(), text.clone()))
            .chain(value.description_items())
            .map(|(name, content)| EventDescription {
                name,
                content,
                ..Default::default()
            })
            .collect(),
        start_time: value.start_time.map(DateTime::from).into(),
        end_time: value
            .start_time
            .zip(value.duration)
            .map(|(start_time, duration)| DateTime::from(start_time + duration))
            .into(),
        service_id: service_id.into(),
        ..Default::default()
    }
}

impl From<NaiveDateTime> for DateTime {
    fn from(value: NaiveDateTime) -> Self {
        // The SI carries JST wall-clock time and the server runs on that zone,
        // so the local offset is the one the broadcast was scheduled against.
        timestamp_in(value, &Local)
    }
}

impl From<chrono::DateTime<Local>> for DateTime {
    fn from(value: chrono::DateTime<Local>) -> Self {
        Self {
            seconds: value.timestamp(),
            nanos: value.timestamp_subsec_nanos(),
            ..Default::default()
        }
    }
}

fn timestamp_in<Tz: TimeZone>(value: NaiveDateTime, timezone: &Tz) -> DateTime {
    let value = value
        .and_local_timezone(timezone.clone())
        .earliest()
        .expect("a broadcast time must exist in the time zone of the server");

    DateTime {
        seconds: value.timestamp(),
        nanos: value.timestamp_subsec_nanos(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, NaiveDate};

    use super::*;

    #[test]
    fn converts_broadcast_time_to_a_unix_timestamp() {
        let jst = FixedOffset::east_opt(9 * 60 * 60).unwrap();
        let local_time = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_nano_opt(18, 30, 0, 123_000_000)
            .unwrap();

        let converted = timestamp_in(local_time, &jst);

        assert_eq!(
            converted.seconds,
            local_time.and_utc().timestamp() - 9 * 60 * 60
        );
        assert_eq!(converted.nanos, 123_000_000);
    }
}
