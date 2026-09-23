use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing::info;

use crate::rpc::ChibitvServiceImpl;
use crate::workspace::Workspace;

/// The path the Connect RPC endpoints are served under.
const RPC_PREFIX: &str = "/api";

pub async fn serve(addr: SocketAddr, state: Arc<Workspace>) -> anyhow::Result<()> {
    let router = app(state);

    let listener = TcpListener::bind(&addr).await?;

    info!("Listening on http://{}", &addr);

    axum::serve(listener, router).await?;

    Ok(())
}

fn app(state: Arc<Workspace>) -> Router {
    let service = ChibitvServiceImpl::new(Arc::clone(&state)).register(connectrpc::Router::new());

    // The RPC service handles every path it is given on its own, so it is
    // nested under a prefix to tell its routes apart from the GUI ones.
    let router = Router::new()
        .route(
            "/api/logos/{stream_id}/{service_id}",
            axum::routing::get(logo),
        )
        .with_state(state)
        .nest_service(RPC_PREFIX, connectrpc::ConnectRpcService::new(service));

    #[cfg(feature = "gui")]
    let router = router.fallback(gui::handle);

    router
}

async fn logo(
    axum::extract::State(state): axum::extract::State<Arc<Workspace>>,
    axum::extract::Path((stream_id, service_id)): axum::extract::Path<(u16, u16)>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    let logo = state
        .store()
        .find_logo(crate::service::ServiceKey {
            stream_id,
            service_id,
        })
        .await;
    match logo {
        Ok(Some(logo)) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=300"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            logo.png,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(?error, "Could not read a station logo");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Serves the GUI built into `gui/dist` from the binary itself.
///
/// Development runs the rsbuild dev server instead, which proxies the RPC
/// requests to this server, so these routes only exist in deployment builds.
#[cfg(feature = "gui")]
mod gui {
    use axum::body::Body;
    use axum::http::{HeaderValue, StatusCode, Uri, header};
    use axum::response::{IntoResponse, Response};
    use rust_embed::Embed;

    #[derive(Embed)]
    #[folder = "../../gui/dist"]
    struct Assets;

    const INDEX_PATH: &str = "index.html";

    /// Assets are emitted with a content hash in their name, so they never
    /// change under the same URL.
    const IMMUTABLE_PREFIX: &str = "static/";

    pub(super) async fn handle(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');

        // Unknown paths fall back to the entry point so that the client-side
        // routes keep working on a reload.
        get(path)
            .or_else(|| get(INDEX_PATH))
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
    }

    fn get(path: &str) -> Option<Response> {
        let path = if path.is_empty() { INDEX_PATH } else { path };
        let file = Assets::get(path)?;
        let content_type = HeaderValue::from_str(file.metadata.mimetype()).ok()?;
        let cache_control = if path.starts_with(IMMUTABLE_PREFIX) {
            HeaderValue::from_static("public, max-age=31536000, immutable")
        } else {
            HeaderValue::from_static("no-cache")
        };

        Some(
            (
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::CACHE_CONTROL, cache_control),
                ],
                Body::from(file.data.into_owned()),
            )
                .into_response(),
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;
    use crate::channel::ChannelInner;
    use crate::event::Event;
    use crate::service::ServiceKey;
    use crate::store::{NewChannel, SectionId, StoredService};

    async fn empty_workspace() -> Arc<Workspace> {
        let store = crate::store::open("sqlite::memory:").await.unwrap();

        Arc::new(Workspace::new(store, vec![], None))
    }

    /// A workspace serving one terrestrial channel per stream given, each
    /// carrying the one service named after it.
    async fn workspace_serving(streams: &[(u16, u16)]) -> Arc<Workspace> {
        let workspace = empty_workspace().await;
        let channels = streams
            .iter()
            .map(|&(stream_id, service_id)| NewChannel {
                name: format!("Channel {stream_id}"),
                inner: ChannelInner::IsdbT {
                    frequency: 470_000_000 + u32::from(stream_id) * 6_000_000,
                    bandwidth_hz: 6_000_000,
                },
                transport_stream_id: Some(stream_id),
                services: vec![StoredService {
                    id: service_id,
                    name: format!("Service {service_id}"),
                    provider_name: String::new(),
                }],
            })
            .collect::<Vec<_>>();
        let Ok(_) = workspace.create_channels(&channels).await else {
            panic!("the channels could not be kept");
        };

        workspace
    }

    #[tokio::test]
    async fn lists_current_programmes_and_logos_restored_after_restart() {
        use crate::store::StoredLogo;
        let directory = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", directory.path().join("guide.db").display());
        let key = ServiceKey {
            stream_id: 1,
            service_id: 101,
        };
        let now = chrono::Local::now().naive_local();
        let store = crate::store::open(&url).await.unwrap();
        store
            .create_channels(&[NewChannel {
                name: "Station".to_string(),
                inner: ChannelInner::IsdbT {
                    frequency: 515_142_857,
                    bandwidth_hz: 6_000_000,
                },
                transport_stream_id: Some(key.stream_id),
                services: vec![StoredService {
                    id: key.service_id,
                    name: "Station".to_string(),
                    provider_name: String::new(),
                }],
            }])
            .await
            .unwrap();
        store
            .save_logo(&StoredLogo {
                key,
                png: b"image".to_vec(),
            })
            .await
            .unwrap();
        store
            .replace_section(
                SectionId {
                    original_network_id: 4,
                    stream_id: 1,
                    service_id: 101,
                    table_id: 0x50,
                    section_number: 0,
                },
                &[Event {
                    start_time: Some(now - chrono::TimeDelta::minutes(1)),
                    duration: Some(chrono::TimeDelta::minutes(30)),
                    name: Some("On air".into()),
                    ..Event::new(key, 7)
                }],
            )
            .await
            .unwrap();
        drop(store);
        let store = crate::store::open(&url).await.unwrap();
        let channels = store
            .load_channels()
            .await
            .unwrap()
            .iter()
            .map(crate::channel::Channel::from)
            .collect();
        let expected_logo = format!(
            "/api/logos/1/101?v={:08x}",
            crate::logo::PNG_CRC.checksum(b"image")
        );
        let response = app(Arc::new(Workspace::new(store, channels, None)))
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ListServices")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 10000).await.unwrap();
        let response: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(response["services"][0]["currentEvent"]["title"], "On air");
        assert_eq!(response["services"][0]["logoUrl"], expected_logo);
    }

    #[tokio::test]
    async fn serves_logo_pngs_and_missing_logos_without_shadowing_rpc() {
        let workspace = workspace_serving(&[(1, 101)]).await;
        workspace
            .store()
            .save_logo(&crate::store::StoredLogo {
                key: ServiceKey {
                    stream_id: 1,
                    service_id: 101,
                },
                png: b"png".to_vec(),
            })
            .await
            .unwrap();
        let router = app(workspace);
        let response = router
            .clone()
            .oneshot(
                Request::get("/api/logos/1/101?v=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            to_bytes(response.into_body(), 100).await.unwrap().as_ref(),
            b"png"
        );
        let response = router
            .oneshot(
                Request::get("/api/logos/2/101")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn serves_connect_json_requests() {
        let response = app(empty_workspace().await)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ListChannels")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"{}");
    }

    #[tokio::test]
    async fn lists_cached_services_from_untuned_channels_by_service_id() {
        // Kept in the reverse order of their keys, which the list is in.
        let workspace = workspace_serving(&[(200, 201), (100, 101)]).await;
        let channel_id = workspace.channels()[0].id;

        let response = app(workspace)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ListServices")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        let service_a = body.find("Service 101").unwrap();
        let service_b = body.find("Service 201").unwrap();
        assert!(service_a < service_b);
        assert!(body.contains(&format!(r#""channelId":{channel_id}"#)));
    }

    #[tokio::test]
    async fn lists_events_from_all_services_when_service_id_is_omitted() {
        let workspace = workspace_serving(&[(1, 101), (2, 201)]).await;
        for (stream_id, service_id, event_id) in [(1, 101, 1001), (2, 201, 2001)] {
            let key = ServiceKey {
                stream_id,
                service_id,
            };
            workspace
                .store()
                .replace_section(
                    SectionId {
                        original_network_id: 1,
                        stream_id,
                        service_id,
                        table_id: 0x50,
                        section_number: 0,
                    },
                    &[Event::new(key, event_id)],
                )
                .await
                .unwrap();
        }

        let response = app(workspace)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ListEvents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""serviceId":101"#));
        assert!(body.contains(r#""serviceId":201"#));
    }

    #[tokio::test]
    async fn lists_the_background_tasks() {
        let response = app(empty_workspace().await)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ListTasks")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"{}");
    }

    #[tokio::test]
    async fn refuses_to_refresh_events_without_a_crawler() {
        let response = app(empty_workspace().await)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/RefreshEvents")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("failed_precondition"));
    }

    #[tokio::test]
    async fn refuses_to_delete_a_task_it_does_not_know() {
        let response = app(empty_workspace().await)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/DeleteTask")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from(r#"{"taskId":"42"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("task not found"));
    }

    #[tokio::test]
    async fn refuses_to_record_without_a_configured_storage() {
        let response = app(empty_workspace().await)
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/ScheduleRecording")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from(
                        r#"{"service":{"streamId":100,"serviceId":101},"eventId":1}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("recording is unavailable"));
    }

    #[cfg(feature = "gui")]
    #[tokio::test]
    async fn serves_the_embedded_gui() {
        let router = app(empty_workspace().await);

        let response = router
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );

        // An unknown path falls back to the entry point of the single page
        // application.
        let response = router
            .oneshot(Request::get("/unknown").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[cfg(not(feature = "gui"))]
    #[tokio::test]
    async fn does_not_serve_legacy_http_api() {
        let response = app(empty_workspace().await)
            .oneshot(Request::get("/api/channels").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
