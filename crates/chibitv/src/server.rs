use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use http_body::Frame;
use http_body_util::StreamBody;
use tokio::net::TcpListener;
use tokio_stream::StreamExt;
use tracing::info;
use utoipa::OpenApi;

use crate::workspace::{Workspace, WorkspaceError};

#[derive(OpenApi)]
#[openapi(
    info(description = "chibitv API"),
    paths(
        get_channels,
        get_services,
        get_events,
        get_stream,
        update_stream,
        get_m2ts_stream,
    )
)]
pub struct ApiDoc;

pub async fn serve(addr: SocketAddr, state: Arc<Workspace>) -> anyhow::Result<()> {
    let router = Router::new()
        .route("/channels", get(get_channels))
        .route("/services", get(get_services))
        .route("/services/{id}/events", get(get_events))
        .route("/streams/{id}", get(get_stream).patch(update_stream))
        .route("/streams/{id}/stream.ts", get(get_m2ts_stream))
        .route("/openapi.json", get(async || Json(ApiDoc::openapi())))
        .with_state(state);

    let router = Router::new().nest("/api", router);

    let listener = TcpListener::bind(&addr).await?;

    info!("Listening on http://{}", &addr);

    axum::serve(listener, router).await?;

    Ok(())
}

mod model {
    use chrono::NaiveDateTime;
    use serde::{Deserialize, Serialize};
    use utoipa::ToSchema;

    use crate::registry;

    #[derive(Default, Serialize, ToSchema)]
    pub struct Channel {
        pub id: usize,
        pub name: String,
    }

    #[derive(Default, Serialize, ToSchema)]
    pub struct Service {
        pub id: u16,
        pub name: String,
        pub provider_name: String,
    }

    impl From<&registry::Service> for Service {
        fn from(value: &registry::Service) -> Self {
            Self {
                id: value.id,
                name: value.name.to_string(),
                provider_name: value.provider_name.to_string(),
            }
        }
    }

    #[derive(Default, Serialize, ToSchema)]
    pub struct EventDescription {
        pub name: String,
        pub content: String,
    }

    #[derive(Default, Serialize, ToSchema)]
    pub struct Event {
        pub id: u16,
        pub title: String,
        pub description: Vec<EventDescription>,
        pub start_time: Option<NaiveDateTime>,
        pub end_time: Option<NaiveDateTime>,
    }

    impl From<&registry::Event> for Event {
        fn from(value: &registry::Event) -> Self {
            Self {
                id: value.id,
                title: value.name.clone().unwrap_or_default(),
                description: value
                    .description
                    .iter()
                    .flatten()
                    .cloned()
                    .map(|(name, content)| EventDescription { name, content })
                    .collect(),
                start_time: value.start_time,
                end_time: value
                    .start_time
                    .zip(value.duration)
                    .map(|(start_time, duration)| start_time + duration),
            }
        }
    }

    #[derive(Default, Serialize, ToSchema)]
    pub struct Stream {
        pub service: Option<Service>,
        pub event: Option<Event>,
    }

    #[derive(Deserialize, ToSchema)]
    pub struct StreamUpdate {
        pub service_id: Option<u16>,
    }
}

#[utoipa::path(
    get,
    path = "/channels",
    responses((status = 200, body = Vec<model::Channel>)),
)]
async fn get_channels(State(workspace): State<Arc<Workspace>>) -> Json<Vec<model::Channel>> {
    let channels = workspace
        .channels()
        .map(|(id, channel)| model::Channel {
            id,
            name: channel.name.to_string(),
        })
        .collect();

    Json(channels)
}

#[utoipa::path(
    get,
    path = "/services",
    responses((status = 200, body = Vec<model::Service>)),
)]
async fn get_services(State(workspace): State<Arc<Workspace>>) -> Json<Vec<model::Service>> {
    let services = workspace
        .registry()
        .get_all_services()
        .iter()
        .map(model::Service::from)
        .collect();

    Json(services)
}

#[utoipa::path(
    get,
    path = "/services/{id}/events",
    params(("id" = u16, Path)),
    responses((status = 200, body = Vec<model::Event>)),
)]
async fn get_events(
    State(workspace): State<Arc<Workspace>>,
    Path(service_id): Path<u16>,
) -> Json<Vec<model::Event>> {
    let events = workspace
        .registry()
        .get_events_by_service_id(service_id)
        .iter()
        .map(model::Event::from)
        .collect();

    Json(events)
}

#[utoipa::path(
    get,
    path = "/streams/{id}",
    params(("id" = u32, Path)),
    responses((status = 200, body = model::Stream), (status = NOT_FOUND)),
)]
async fn get_stream(
    State(workspace): State<Arc<Workspace>>,
    Path(stream_id): Path<u32>,
) -> Result<Json<model::Stream>, StatusCode> {
    let Some((service, event)) = workspace.get_current_event(stream_id) else {
        return Err(StatusCode::NOT_FOUND);
    };

    let stream = model::Stream {
        service: service.map(|service| model::Service::from(&service)),
        event: event.map(|event| model::Event::from(&event)),
    };

    Ok(Json(stream))
}

#[utoipa::path(
    patch,
    path = "/streams/{id}",
    params(("id" = u32, Path)),
    request_body = model::StreamUpdate,
    responses((status = 204), (status = BAD_REQUEST), (status = NOT_FOUND)),
)]
async fn update_stream(
    State(workspace): State<Arc<Workspace>>,
    Path(stream_id): Path<u32>,
    Json(request): Json<model::StreamUpdate>,
) -> Result<(), StatusCode> {
    if let Some(service_id) = request.service_id {
        workspace
            .set_channel(stream_id, service_id)
            .map_err(|err| match err {
                WorkspaceError::ChannelNotFound
                | WorkspaceError::ServiceNotFound
                | WorkspaceError::StreamNotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            })?;
    }

    Ok(())
}

#[utoipa::path(
    get,
    path = "/streams/{id}/stream.ts",
    responses((status = 200, content_type = "video/mp2t"), (status = NOT_FOUND)),
    params(("id" = u32, Path)),
)]
async fn get_m2ts_stream(
    State(workspace): State<Arc<Workspace>>,
    Path(stream_id): Path<u32>,
) -> Result<Response, StatusCode> {
    let stream = workspace
        .get_m2ts_stream(stream_id)
        .ok_or(StatusCode::NOT_FOUND)?
        .filter_map(|data| data.ok().map(Frame::data))
        .map(Ok::<_, Infallible>);

    Ok(Response::builder()
        .header("Content-Type", "video/mp2t")
        .body(Body::new(StreamBody::new(stream)))
        .unwrap())
}
