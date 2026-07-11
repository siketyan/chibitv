use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;
use tracing::info;

use crate::rpc::ChibitvServiceImpl;
use crate::workspace::Workspace;

pub async fn serve(addr: SocketAddr, state: Arc<Workspace>) -> anyhow::Result<()> {
    let router = app(state);

    let listener = TcpListener::bind(&addr).await?;

    info!("Listening on http://{}", &addr);

    axum::serve(listener, router).await?;

    Ok(())
}

fn app(state: Arc<Workspace>) -> Router {
    ChibitvServiceImpl::new(state)
        .register(connectrpc::Router::new())
        .into_axum_router()
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode, header};
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
                Request::post("/chibitv.v1.ChibitvService/ListChannels")
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
                Request::post("/chibitv.v1.ChibitvService/ListServices")
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
    }

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
                Request::post("/chibitv.v1.ChibitvService/GetStream")
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
