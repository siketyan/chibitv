use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};
use connectrpc::{
    ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};
use tracing::warn;

use crate::channel::ChannelInner;
use crate::event;
use crate::proto::chibitv::v1::*;
use crate::scanner::{ScanDeliverySystem, ScanRequest};
use crate::service;
use crate::service_information::Signal;
use crate::store;
use crate::stream::{StreamFailure, StreamFailureKind};
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

    /// The broadcast wave the service of the key is carried on, when it is one
    /// of the configured channels that carries it.
    fn wave_of(&self, key: service::ServiceKey) -> Option<DeliverySystem> {
        self.workspace
            .channel_of(key)
            .map(|channel| delivery_system(&channel.inner))
    }
}

#[allow(refining_impl_trait)]
impl ChibitvService for ChibitvServiceImpl {
    async fn list_channels(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListChannelsRequest>,
    ) -> ServiceResult<ListChannelsResponse> {
        Response::ok(ListChannelsResponse {
            channels: self.workspace.channels().iter().map(channel).collect(),
            ..Default::default()
        })
    }

    async fn list_services(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, ListServicesRequest>,
    ) -> ServiceResult<ListServicesResponse> {
        let store = self.workspace.store();
        let now = Local::now().naive_local();
        // The logos are read in one statement rather than one per service.
        let logos = store
            .find_logos()
            .await
            .map_err(store_error)?
            .into_iter()
            .map(|logo| (logo.key, logo.png))
            .collect::<HashMap<_, _>>();

        let mut services = vec![];
        for (service, channel) in self.workspace.services().await.map_err(workspace_error)? {
            let mut message = service_message(&service, &channel);
            message.current_event = store
                .find_event_on_air(service.key, now)
                .await
                .map_err(store_error)?
                .as_ref()
                .map(event_message)
                .into();
            message.logo_url = logos
                .get(&service.key)
                .map(|png| logo_url(service.key, png))
                .unwrap_or_default();
            services.push(message);
        }

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
        // A wave the request does not name is every one of them, which is what
        // a guide showing them together asks for.
        let wave = match request.delivery_system.as_known() {
            Some(DeliverySystem::Unspecified) => None,
            Some(known) => Some(known),
            None => {
                return Err(ConnectError::invalid_argument(
                    "delivery_system is not one this server knows",
                ));
            }
        };

        let keys = if let Some(key) = request.service.as_option() {
            vec![service_key(key)?]
        } else {
            self.workspace
                .services()
                .await
                .map_err(workspace_error)?
                .into_iter()
                .map(|(service, _)| service.key)
                .collect()
        };

        let mut events = vec![];
        for key in keys {
            // A service no channel being served carries sits on no known
            // wave, so a request for one leaves it out rather than guessing.
            if wave.is_some_and(|wave| self.wave_of(key) != Some(wave)) {
                continue;
            }

            events.extend(
                self.workspace
                    .store()
                    .find_events(key)
                    .await
                    .map_err(store_error)?,
            );
        }
        let events = events.iter().map(event_message).collect();

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

    async fn scan_channels(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ScanChannelsRequest>,
    ) -> ServiceResult<ScanChannelsResponse> {
        const DEFAULT_TIMEOUT_SECONDS: u32 = 12;
        const MAX_TIMEOUT_SECONDS: u32 = 120;

        let timeout_seconds = match request.timeout_seconds {
            0 => DEFAULT_TIMEOUT_SECONDS,
            seconds if seconds <= MAX_TIMEOUT_SECONDS => seconds,
            _ => {
                return Err(ConnectError::invalid_argument(
                    "timeout_seconds must be at most 120",
                ));
            }
        };
        let delivery_system = match request.delivery_system.as_known() {
            Some(DeliverySystem::IsdbS) => ScanDeliverySystem::IsdbS,
            Some(DeliverySystem::IsdbS3) => ScanDeliverySystem::IsdbS3,
            // The terrestrial channels are what a request that says nothing
            // asks for, as they are what the command scans by default.
            Some(DeliverySystem::Unspecified | DeliverySystem::IsdbT) => ScanDeliverySystem::IsdbT,
            None => {
                return Err(ConnectError::invalid_argument(
                    "delivery_system is not one this server knows",
                ));
            }
        };

        let task = self
            .workspace
            .scan_channels(ScanRequest {
                delivery_system,
                fast: request.fast,
                timeout: Duration::from_secs(u64::from(timeout_seconds)),
                ..ScanRequest::default()
            })
            .map_err(workspace_error)?;

        Response::ok(ScanChannelsResponse {
            task: Some(task_message(&task)).into(),
            ..Default::default()
        })
    }

    async fn get_scan_result(
        &self,
        _ctx: RequestContext,
        _request: ServiceRequest<'_, GetScanResultRequest>,
    ) -> ServiceResult<GetScanResultResponse> {
        let found = self.workspace.scan_result();

        Response::ok(GetScanResultResponse {
            channels: found.iter().map(new_channel_message).collect(),
            ..Default::default()
        })
    }

    async fn bulk_create_channels(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, BulkCreateChannelsRequest>,
    ) -> ServiceResult<BulkCreateChannelsResponse> {
        // Every channel is read before any of them is kept, so a request one
        // of them is wrong in keeps none of it.
        let channels = request
            .channels
            .iter()
            .map(new_channel)
            .collect::<Result<Vec<_>, _>>()?;
        let channels = self
            .workspace
            .create_channels(&channels)
            .await
            .map_err(workspace_error)?;

        Response::ok(BulkCreateChannelsResponse {
            channels: channels.iter().map(channel).collect(),
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
    ) -> ServiceResult<ServiceStream<TaskEvent>> {
        let tasks = self.workspace.tasks();
        // The updates are subscribed to before the tasks are listed, so that a
        // task changing in between is reported rather than missed. A client
        // keeps the tasks by their identifier, so seeing one twice is harmless.
        let updates = BroadcastStream::new(tasks.subscribe());
        let current = tokio_stream::iter(tasks.list().into_iter().map(task::TaskUpdate::Changed));

        Response::stream_ok(
            current
                .chain(updates.filter_map(|update| update.ok()))
                .map(|update| Ok(task_event(update))),
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

    async fn delete_task(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, DeleteTaskRequest>,
    ) -> ServiceResult<DeleteTaskResponse> {
        self.workspace
            .delete_task(request.task_id)
            .map_err(workspace_error)?;

        Response::ok(DeleteTaskResponse::default())
    }

    async fn schedule_recording(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, ScheduleRecordingRequest>,
    ) -> ServiceResult<ScheduleRecordingResponse> {
        let key = service_key(
            request
                .service
                .as_option()
                .ok_or_else(|| ConnectError::invalid_argument("service is required"))?,
        )?;
        let event_id = u16::try_from(request.event_id)
            .map_err(|_| ConnectError::invalid_argument("event_id is out of range"))?;

        let task = self
            .workspace
            .schedule_recording(key, event_id)
            .await
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
        let key = service_key(
            request
                .service
                .as_option()
                .ok_or_else(|| ConnectError::invalid_argument("service is required"))?,
        )?;

        let StreamSubscription {
            stream,
            init_segment,
            fmp4,
            signals,
            failure,
            failures,
        } = self
            .workspace
            .subscribe_stream(key)
            .await
            .map_err(workspace_error)?;

        let initial_state =
            tokio_stream::iter([stream_state(&self.workspace, &stream, None).await]);
        let init_segment = tokio_stream::iter(init_segment.into_iter().map(fmp4_response));
        // A client that falls behind loses whole fragments, which leaves a hole
        // in the byte stream its decoder is reading; it recovers by starting the
        // stream over, so all that is left to do here is say what happened.
        let fmp4 = fmp4.filter_map(move |data| match data {
            Ok(data) => Some(fmp4_response(data)),
            Err(BroadcastStreamRecvError::Lagged(count)) => {
                warn!(
                    service_id = key.service_id,
                    count, "An fMP4 stream client fell behind"
                );
                None
            }
        });
        let states = {
            let workspace = Arc::clone(&self.workspace);
            let stream = Arc::clone(&stream);
            signals
                .then(move |signal| {
                    let workspace = Arc::clone(&workspace);
                    let stream = Arc::clone(&stream);

                    async move {
                        match signal.ok()? {
                            Signal::EventChanged { event_id } => {
                                Some(stream_state(&workspace, &stream, Some(event_id)).await)
                            }
                        }
                    }
                })
                .filter_map(|state| state)
        };

        // What stopped the stream, once something has. Nothing else ends the
        // call: the channels it reads stay open for as long as the stream held
        // below is alive.
        let failure = async move {
            if let Some(failure) = failure {
                return failure;
            }

            let mut failures = failures;
            loop {
                match failures.next().await {
                    Some(Ok(failure)) => return failure,
                    Some(Err(BroadcastStreamRecvError::Lagged(_))) => continue,
                    None => std::future::pending().await,
                }
            }
        };

        // The stream keeps the tuner occupied, so it is moved into the
        // response stream to release the tuner once every client is gone.
        Response::stream_ok(
            initial_state
                .chain(ending_with_failure(
                    init_segment.chain(fmp4).merge(states),
                    failure,
                ))
                .map(move |response| {
                    let _stream = &stream;
                    Ok(response)
                }),
        )
    }
}

fn task_event(value: task::TaskUpdate) -> TaskEvent {
    TaskEvent {
        payload: Some(match value {
            task::TaskUpdate::Changed(task) => {
                task_event::Payload::Changed(Box::new(task_message(&task)))
            }
            task::TaskUpdate::Deleted(id) => task_event::Payload::Deleted(id),
        }),
        ..Default::default()
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
        task::TaskKind::ScanChannels => TaskKind::ScanChannels,
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

async fn stream_state(
    workspace: &Workspace,
    stream: &crate::stream::Stream,
    event_id: Option<u16>,
) -> StreamResponse {
    let key = stream.key();
    // What cannot be read is left out of the state rather than ending the
    // stream, which is still worth watching without it.
    let service = workspace.service(key).await.ok().flatten();
    let event = match event_id.or_else(|| stream.event_id()) {
        Some(event_id) if service.is_some() => workspace
            .store()
            .find_event(key, event_id)
            .await
            .inspect_err(|error| warn!(%error, "Could not read the event on air"))
            .ok()
            .flatten(),
        _ => None,
    };

    StreamResponse {
        payload: Some(stream_response::Payload::State(Box::new(StreamState {
            service: service
                .as_ref()
                .map(|(service, channel)| service_message(service, channel))
                .into(),
            event: event.as_ref().map(event_message).into(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}

/// A response stream that ends on the failure stopping the stream behind it,
/// which it hands over as its last message.
///
/// Nothing more is coming by then — a card that hands over no key answers the
/// next ECM the same way — so the call ends rather than leaving the client to
/// sit through its own timeout before taking the stream up again.
struct EndingWithFailure {
    body: Pin<Box<dyn Stream<Item = StreamResponse> + Send>>,
    /// Taken once the failure has been sent, which is what ends the stream.
    failure: Option<Pin<Box<dyn Future<Output = StreamFailure> + Send>>>,
}

impl Stream for EndingWithFailure {
    type Item = StreamResponse;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(failure) = this.failure.as_mut() else {
            return Poll::Ready(None);
        };

        // The failure comes first: whatever the body has left to say is media
        // from before the pipeline stopped, and there is nobody to play it.
        if let Poll::Ready(failure) = failure.as_mut().poll(cx) {
            this.failure = None;
            return Poll::Ready(Some(failure_response(failure)));
        }

        this.body.as_mut().poll_next(cx)
    }
}

fn ending_with_failure(
    body: impl Stream<Item = StreamResponse> + Send + 'static,
    failure: impl Future<Output = StreamFailure> + Send + 'static,
) -> impl Stream<Item = StreamResponse> + Send + 'static {
    EndingWithFailure {
        body: Box::pin(body),
        failure: Some(Box::pin(failure)),
    }
}

fn failure_response(failure: StreamFailure) -> StreamResponse {
    StreamResponse {
        payload: Some(stream_response::Payload::Error(Box::new(StreamError {
            kind: stream_error_kind(failure.kind).into(),
            message: failure.message,
            ..Default::default()
        }))),
        ..Default::default()
    }
}

fn stream_error_kind(value: StreamFailureKind) -> StreamErrorKind {
    match value {
        StreamFailureKind::NotContracted => StreamErrorKind::NotContracted,
        StreamFailureKind::DescramblingRefused => StreamErrorKind::DescramblingRefused,
        StreamFailureKind::Internal => StreamErrorKind::Internal,
    }
}

fn fmp4_response(data: bytes::Bytes) -> StreamResponse {
    StreamResponse {
        payload: Some(stream_response::Payload::Fmp4(data.to_vec())),
        ..Default::default()
    }
}

fn channel(channel: &crate::channel::Channel) -> Channel {
    Channel {
        id: channel.id as u32,
        name: channel.name.clone(),
        delivery_system: delivery_system(&channel.inner).into(),
        ..Default::default()
    }
}

fn delivery_system(inner: &ChannelInner) -> DeliverySystem {
    match inner {
        ChannelInner::IsdbT { .. } | ChannelInner::BonIsdbT { .. } => DeliverySystem::IsdbT,
        ChannelInner::IsdbS { .. } | ChannelInner::BonIsdbS { .. } => DeliverySystem::IsdbS,
        ChannelInner::IsdbS3 { .. } | ChannelInner::BonIsdbS3 { .. } => DeliverySystem::IsdbS3,
    }
}

/// The bandwidth a terrestrial channel is, which a request may leave out.
const TERRESTRIAL_BANDWIDTH_HZ: u32 = 6_000_000;

/// A channel a scan found, as the message that keeping it hands back.
fn new_channel_message(channel: &store::NewChannel) -> NewChannel {
    let tuning = match channel.inner {
        ChannelInner::IsdbT {
            frequency,
            bandwidth_hz,
        } => new_channel::Tuning::Parameters(Box::new(TuningParameters {
            frequency,
            bandwidth_hz,
            ..Default::default()
        })),
        ChannelInner::IsdbS {
            frequency,
            stream_id,
        }
        | ChannelInner::IsdbS3 {
            frequency,
            stream_id,
        } => new_channel::Tuning::Parameters(Box::new(TuningParameters {
            frequency,
            stream_id: Some(stream_id),
            ..Default::default()
        })),
        // A scan finds no BonDriver channel: the driver enumerates those.
        ChannelInner::BonIsdbT { space, channel }
        | ChannelInner::BonIsdbS { space, channel }
        | ChannelInner::BonIsdbS3 { space, channel } => {
            new_channel::Tuning::Bondriver(Box::new(BonDriverChannel {
                space,
                channel,
                ..Default::default()
            }))
        }
    };

    NewChannel {
        name: channel.name.clone(),
        delivery_system: delivery_system(&channel.inner).into(),
        tuning: Some(tuning),
        transport_stream_id: channel.transport_stream_id.map(u32::from),
        services: channel
            .services
            .iter()
            .map(|service| NewService {
                id: u32::from(service.id),
                name: service.name.clone(),
                provider_name: service.provider_name.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

/// A channel a request asks to keep, as the store takes it.
fn new_channel(channel: &NewChannelView<'_>) -> Result<store::NewChannel, ConnectError> {
    if channel.name.is_empty() {
        return Err(ConnectError::invalid_argument("a channel needs a name"));
    }

    let Some(delivery_system) = channel.delivery_system.as_known() else {
        return Err(ConnectError::invalid_argument(
            "delivery_system is not one this server knows",
        ));
    };
    let inner = match (delivery_system, &channel.tuning) {
        (_, None) => {
            return Err(ConnectError::invalid_argument(
                "a channel needs the tuning it is reached with",
            ));
        }
        (DeliverySystem::Unspecified, _) => {
            return Err(ConnectError::invalid_argument(
                "a channel needs the broadcast it is carried on",
            ));
        }
        (DeliverySystem::IsdbT, Some(new_channel::TuningView::Parameters(parameters))) => {
            ChannelInner::IsdbT {
                frequency: frequency_of(parameters)?,
                bandwidth_hz: match parameters.bandwidth_hz {
                    0 => TERRESTRIAL_BANDWIDTH_HZ,
                    bandwidth_hz => bandwidth_hz,
                },
            }
        }
        (DeliverySystem::IsdbS, Some(new_channel::TuningView::Parameters(parameters))) => {
            ChannelInner::IsdbS {
                frequency: frequency_of(parameters)?,
                stream_id: stream_id_of(parameters)?,
            }
        }
        (DeliverySystem::IsdbS3, Some(new_channel::TuningView::Parameters(parameters))) => {
            ChannelInner::IsdbS3 {
                frequency: frequency_of(parameters)?,
                stream_id: stream_id_of(parameters)?,
            }
        }
        (DeliverySystem::IsdbT, Some(new_channel::TuningView::Bondriver(bondriver))) => {
            ChannelInner::BonIsdbT {
                space: bondriver.space,
                channel: bondriver.channel,
            }
        }
        (DeliverySystem::IsdbS, Some(new_channel::TuningView::Bondriver(bondriver))) => {
            ChannelInner::BonIsdbS {
                space: bondriver.space,
                channel: bondriver.channel,
            }
        }
        (DeliverySystem::IsdbS3, Some(new_channel::TuningView::Bondriver(bondriver))) => {
            ChannelInner::BonIsdbS3 {
                space: bondriver.space,
                channel: bondriver.channel,
            }
        }
    };

    Ok(store::NewChannel {
        name: channel.name.to_string(),
        inner,
        transport_stream_id: channel
            .transport_stream_id
            .map(|stream_id| {
                u16::try_from(stream_id).map_err(|_| {
                    ConnectError::invalid_argument("transport_stream_id is not a stream id")
                })
            })
            .transpose()?,
        services: channel
            .services
            .iter()
            .map(|service| {
                Ok(store::StoredService {
                    id: u16::try_from(service.id).map_err(|_| {
                        ConnectError::invalid_argument("a service id is not a service id")
                    })?,
                    name: service.name.to_string(),
                    provider_name: service.provider_name.to_string(),
                })
            })
            .collect::<Result<Vec<_>, ConnectError>>()?,
    })
}

/// The frequency of a tuning, which is no tuning at all without one.
fn frequency_of(parameters: &TuningParametersView<'_>) -> Result<u32, ConnectError> {
    match parameters.frequency {
        0 => Err(ConnectError::invalid_argument(
            "a tuning needs the frequency to tune to",
        )),
        frequency => Ok(frequency),
    }
}

/// The stream a satellite channel is picked out of its transponder by, which
/// the tuning of one has to name.
fn stream_id_of(parameters: &TuningParametersView<'_>) -> Result<u32, ConnectError> {
    parameters.stream_id.ok_or_else(|| {
        ConnectError::invalid_argument("a satellite channel needs the stream it is picked by")
    })
}

fn workspace_error(error: WorkspaceError) -> ConnectError {
    match error {
        WorkspaceError::ServiceNotFound => ConnectError::not_found("service not found"),
        WorkspaceError::TunerBusy => ConnectError::resource_exhausted("all tuners are in use"),
        WorkspaceError::NoTuner(system) => {
            ConnectError::failed_precondition(format!("no tuner receives {system}"))
        }
        WorkspaceError::StreamingUnavailable => {
            ConnectError::unavailable("streaming is unavailable")
        }
        WorkspaceError::EventCrawlerUnavailable => {
            ConnectError::failed_precondition("event crawler is unavailable")
        }
        WorkspaceError::ScannerUnavailable => {
            ConnectError::failed_precondition("scanning is unavailable")
        }
        WorkspaceError::ScanNotPossible(error) => {
            ConnectError::invalid_argument(format!("{error:#}"))
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
        WorkspaceError::TaskNotFinished => {
            ConnectError::failed_precondition("this task has not finished yet")
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

/// Reads a service the caller named, which has to fit what the SI numbers a
/// service with.
fn store_error(error: anyhow::Error) -> ConnectError {
    tracing::error!(?error, "Could not read the database");
    ConnectError::internal("could not read the database")
}

fn service_key(value: &ServiceKeyView<'_>) -> Result<service::ServiceKey, ConnectError> {
    Ok(service::ServiceKey {
        stream_id: u16::try_from(value.stream_id)
            .map_err(|_| ConnectError::invalid_argument("stream_id is out of range"))?,
        service_id: u16::try_from(value.service_id)
            .map_err(|_| ConnectError::invalid_argument("service_id is out of range"))?,
    })
}

fn service_key_message(value: service::ServiceKey) -> ServiceKey {
    ServiceKey {
        stream_id: value.stream_id.into(),
        service_id: value.service_id.into(),
        ..Default::default()
    }
}

/// Where the logo of the service is served from, which changes with the
/// logo so that a client caching the old one asks for the new one.
fn logo_url(key: service::ServiceKey, png: &[u8]) -> String {
    format!(
        "/api/logos/{}/{}?v={:08x}",
        key.stream_id,
        key.service_id,
        crate::service_information::logo::PNG_CRC.checksum(png)
    )
}

fn service_message(service: &service::Service, channel: &crate::channel::Channel) -> Service {
    Service {
        key: Some(service_key_message(service.key)).into(),
        name: service.name.clone(),
        provider_name: service.provider_name.clone(),
        channel_id: channel.id as u32,
        ..Default::default()
    }
}

fn event_message(value: &event::Event) -> Event {
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
        service: Some(service_key_message(value.key)).into(),
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

    use crate::channel::Channel;

    use super::*;

    // The workspace starts a scheduler of its own, so it wants a runtime.
    #[tokio::test]
    async fn reports_the_wave_a_service_is_carried_on() {
        const CARRIED: service::ServiceKey = service::ServiceKey {
            stream_id: 0x1234,
            service_id: 0x5678,
        };
        const UNKNOWN: service::ServiceKey = service::ServiceKey {
            stream_id: 0x4321,
            service_id: 0x8765,
        };

        let store = crate::store::open("sqlite::memory:").await.unwrap();
        let channel = Channel {
            id: 0,
            name: "UHF 20".to_string(),
            inner: ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            },
            stream_id: Some(CARRIED.stream_id),
        };
        let service = ChibitvServiceImpl::new(Arc::new(Workspace::new(store, vec![channel], None)));

        assert_eq!(service.wave_of(CARRIED), Some(DeliverySystem::IsdbT));
        // A service no channel carries belongs to no wave, rather than to the
        // first one that happens to be configured.
        assert_eq!(service.wave_of(UNKNOWN), None);
    }

    #[test]
    fn reports_the_delivery_system_of_every_kind_of_channel() {
        let of = |inner| delivery_system(&inner);

        assert_eq!(
            of(ChannelInner::IsdbT {
                frequency: 515_142_857,
                bandwidth_hz: 6_000_000,
            }),
            DeliverySystem::IsdbT,
        );
        assert_eq!(
            of(ChannelInner::IsdbS {
                frequency: 1_049_480,
                stream_id: 0x4031,
            }),
            DeliverySystem::IsdbS,
        );
        assert_eq!(
            of(ChannelInner::IsdbS3 {
                frequency: 1_318_000,
                stream_id: 0x40F1,
            }),
            DeliverySystem::IsdbS3,
        );
        assert_eq!(
            of(ChannelInner::BonIsdbT {
                space: 0,
                channel: 0,
            }),
            DeliverySystem::IsdbT,
        );
        assert_eq!(
            of(ChannelInner::BonIsdbS {
                space: 0,
                channel: 1,
            }),
            DeliverySystem::IsdbS,
        );
        assert_eq!(
            of(ChannelInner::BonIsdbS3 {
                space: 0,
                channel: 2,
            }),
            DeliverySystem::IsdbS3,
        );
    }

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

    #[tokio::test]
    async fn ends_a_response_stream_on_the_failure_that_stopped_the_stream() {
        let (stopped, failure) = tokio::sync::oneshot::channel();
        // A live stream does not end of its own accord, so neither does this.
        let body = tokio_stream::iter([fmp4_response(bytes::Bytes::from_static(b"a fragment"))])
            .chain(tokio_stream::pending());
        let mut responses =
            std::pin::pin!(ending_with_failure(
                body,
                async move { failure.await.unwrap() }
            ));

        assert!(matches!(
            responses.next().await.unwrap().payload,
            Some(stream_response::Payload::Fmp4(_))
        ));

        stopped
            .send(StreamFailure {
                kind: StreamFailureKind::NotContracted,
                message: "the card holds no contract for this programme".to_string(),
            })
            .unwrap();

        let Some(stream_response::Payload::Error(error)) = responses.next().await.unwrap().payload
        else {
            panic!("the failure should be the next response");
        };
        assert_eq!(error.kind, StreamErrorKind::NotContracted);
        assert!(responses.next().await.is_none());
    }
}
