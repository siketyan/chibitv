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
    let service = ChibitvServiceImpl::new(state).register(connectrpc::Router::new());

    // The RPC service handles every path it is given on its own, so it is
    // nested under a prefix to tell its routes apart from the GUI ones.
    let router =
        Router::new().nest_service(RPC_PREFIX, connectrpc::ConnectRpcService::new(service));

    #[cfg(feature = "gui")]
    let router = router.fallback(gui::handle);

    router
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
    use std::sync::RwLock;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
    use chibitv_b10::table::EventInformation;
    use tower::ServiceExt;

    use super::*;
    use crate::registry::Registry;
    use crate::stream::Streams;

    fn empty_workspace() -> Arc<Workspace> {
        Arc::new(Workspace::new(
            Arc::new(Registry::default()),
            vec![],
            RwLock::new(Streams::new()),
        ))
    }

    #[tokio::test]
    async fn serves_connect_json_requests() {
        let response = app(empty_workspace())
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
        let registry = Arc::new(Registry::default());
        registry.put_cached_service(
            1,
            200,
            201,
            "Service B".to_string(),
            "Provider B".to_string(),
        );
        registry.put_cached_service(
            0,
            100,
            101,
            "Service A".to_string(),
            "Provider A".to_string(),
        );
        let workspace = Arc::new(Workspace::new(
            registry,
            vec![],
            RwLock::new(Streams::new()),
        ));

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
        let service_a = body.find("Service A").unwrap();
        let service_b = body.find("Service B").unwrap();
        assert!(service_a < service_b);
        assert!(body.contains(r#""channelId":1"#));
    }

    #[tokio::test]
    async fn lists_events_from_all_services_when_service_id_is_omitted() {
        let registry = Arc::new(Registry::default());
        for (channel_id, service_id, event_id) in [(0, 101, 1001), (1, 201, 2001)] {
            registry.put_cached_service(
                channel_id,
                channel_id as u16,
                service_id,
                format!("Service {service_id}"),
                String::new(),
            );
            registry.put_b10_event(
                service_id,
                &EventInformation {
                    event_id,
                    start_time: None,
                    duration: None,
                    running_status: 0,
                    free_ca_mode: false,
                    descriptors: vec![],
                },
            );
        }
        let workspace = Arc::new(Workspace::new(
            registry,
            vec![],
            RwLock::new(Streams::new()),
        ));

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

    #[cfg(feature = "gui")]
    #[tokio::test]
    async fn serves_the_embedded_gui() {
        let router = app(empty_workspace());

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
        let response = app(empty_workspace())
            .oneshot(Request::get("/api/channels").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn maps_missing_stream_to_connect_not_found() {
        let response = app(empty_workspace())
            .oneshot(
                Request::post("/api/chibitv.v1.ChibitvService/GetStream")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("connect-protocol-version", "1")
                    .body(Body::from(r#"{"streamId":99}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#""code":"not_found""#));
    }
}
