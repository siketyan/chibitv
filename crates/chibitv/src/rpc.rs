use std::sync::Arc;
use std::time::Duration;

use chrono::{FixedOffset, NaiveDateTime};
use connectrpc::{
    ConnectError, RequestContext, Response, Router, ServiceRequest, ServiceResult, ServiceStream,
};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::event_crawler::CrawledEvent;
use crate::proto::chibitv::v1::*;
use crate::registry;
use crate::service_information::Signal;
use crate::workspace::{Workspace, WorkspaceError};

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
    ) -> ServiceResult<ServiceStream<Event>> {
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
        let crawler = self
            .workspace
            .event_crawler()
            .ok_or_else(|| ConnectError::failed_precondition("event crawler is unavailable"))?;
        let channels = self
            .workspace
            .channels()
            .map(|(_, channel)| channel.clone())
            .collect::<Vec<_>>();
        let registry = self.workspace.registry_arc();
        let (tx, rx) = tokio::sync::mpsc::channel(16);

        std::thread::spawn(move || {
            let result = crawler.crawl(
                &channels,
                registry,
                Duration::from_secs(u64::from(dwell_time_seconds)),
                |event| tx.blocking_send(Ok(crawled_event_message(event))).is_ok(),
            );

            if let Err(error) = result {
                tracing::error!(%error, "Event refresh failed");
                let _ = tx.blocking_send(Err(ConnectError::unavailable("event refresh failed")));
            }
        });

        Response::stream_ok(ReceiverStream::new(rx))
    }

    async fn get_stream(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, GetStreamRequest>,
    ) -> ServiceResult<StreamState> {
        let (service, event) = self
            .workspace
            .get_current_event(request.stream_id)
            .ok_or_else(|| ConnectError::not_found("stream not found"))?;

        Response::ok(StreamState {
            service: service.as_ref().map(Service::from).into(),
            event: service
                .as_ref()
                .zip(event.as_ref())
                .map(|(service, event)| event_message(service.id, event))
                .into(),
            ..Default::default()
        })
    }

    async fn update_stream(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, UpdateStreamRequest>,
    ) -> ServiceResult<UpdateStreamResponse> {
        if let Some(service_id) = request.service_id {
            let service_id = u16::try_from(service_id)
                .map_err(|_| ConnectError::invalid_argument("service_id is out of range"))?;
            self.workspace
                .set_channel(request.stream_id, service_id)
                .map_err(workspace_error)?;
        }

        Response::ok(UpdateStreamResponse::default())
    }

    async fn stream(
        &self,
        _ctx: RequestContext,
        request: ServiceRequest<'_, StreamRequest>,
    ) -> ServiceResult<ServiceStream<StreamResponse>> {
        let stream_id = request.stream_id;
        let (init_segment, fmp4, signals) = self
            .workspace
            .subscribe_stream(stream_id)
            .ok_or_else(|| ConnectError::not_found("stream not found"))?;
        let initial_state = stream_state(&self.workspace, stream_id)
            .ok_or_else(|| ConnectError::not_found("stream not found"))?;

        let initial_state = tokio_stream::iter([initial_state]);
        let init_segment = tokio_stream::iter(init_segment.into_iter().map(fmp4_response));
        let fmp4 = fmp4.filter_map(|data| data.ok().map(fmp4_response));
        let workspace = Arc::clone(&self.workspace);
        let states = signals.filter_map(move |signal| match signal.ok()? {
            Signal::EventChanged { event_id } => {
                stream_state_with_id(&workspace, stream_id, Some(event_id))
            }
            Signal::ChannelChanged { .. } => stream_state(&workspace, stream_id),
        });

        Response::stream_ok(
            initial_state
                .chain(init_segment.chain(fmp4).merge(states))
                .map(Ok),
        )
    }
}

fn crawled_event_message(value: CrawledEvent) -> Event {
    event_message(value.service_id, &value.event)
}

fn stream_state(workspace: &Workspace, stream_id: u32) -> Option<StreamResponse> {
    stream_state_with_id(workspace, stream_id, None)
}

fn stream_state_with_id(
    workspace: &Workspace,
    stream_id: u32,
    event_id: Option<u16>,
) -> Option<StreamResponse> {
    let (service, event) = workspace.get_current_event_with_id(stream_id, event_id)?;

    Some(StreamResponse {
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
    })
}

fn fmp4_response(data: bytes::Bytes) -> StreamResponse {
    StreamResponse {
        payload: Some(stream_response::Payload::Fmp4(data.to_vec())),
        ..Default::default()
    }
}

fn workspace_error(error: WorkspaceError) -> ConnectError {
    match error {
        WorkspaceError::ChannelNotFound => ConnectError::not_found("channel not found"),
        WorkspaceError::ServiceNotFound => ConnectError::not_found("service not found"),
        WorkspaceError::StreamNotFound => ConnectError::not_found("stream not found"),
        WorkspaceError::Internal(error) => {
            tracing::error!(?error, "Failed to update stream");
            ConnectError::internal("failed to update stream")
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
        description: value
            .description
            .iter()
            .flatten()
            .cloned()
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
        let jst = FixedOffset::east_opt(9 * 60 * 60).expect("JST offset must be valid");
        let value = value
            .and_local_timezone(jst)
            .single()
            .expect("a fixed timezone offset must be unambiguous");

        Self {
            seconds: value.timestamp(),
            nanos: value.timestamp_subsec_nanos(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    #[test]
    fn converts_broadcast_time_from_jst_to_unix_timestamp() {
        let local_time = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_nano_opt(18, 30, 0, 123_000_000)
            .unwrap();

        let converted = DateTime::from(local_time);

        assert_eq!(
            converted.seconds,
            local_time.and_utc().timestamp() - 9 * 60 * 60
        );
        assert_eq!(converted.nanos, 123_000_000);
    }
}
