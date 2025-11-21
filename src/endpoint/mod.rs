mod execute_config;

use crate::endpoint::execute_config::execute_config;
use crate::{app::config::Config, checker::HttpChecker};
use axum::{routing::post, Router};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct EndpointState {
    checker: Arc<HttpChecker>,
    region: &'static str,
}

fn new_router(checker: Arc<HttpChecker>, region: &'static str) -> Router {
    Router::new()
        .route("/execute_config", post(execute_config))
        .with_state(Arc::new(EndpointState { checker, region }))
}

pub fn start_endpoint(
    config: &Arc<Config>,
    shutdown_signal: CancellationToken,
    checker: Arc<HttpChecker>,
) -> JoinHandle<()> {
    let app = new_router(checker, config.region);

    let endpoint_port = config.webserver_port;

    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(format!("localhost:{}", endpoint_port)).await;

        let Ok(listener) = listener else {
            tracing::error!(
                "Could not listen on webserver endpoint: {}",
                listener.err().unwrap().to_string()
            );
            return;
        };
        let shutdown_fut = shutdown_signal.cancelled_owned();
        let result = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_fut)
            .await;
        if let Err(e) = result {
            tracing::warn!("Error while running webserver: {}", e.to_string());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        assertions::{Assertion, Op},
        checker::dummy_checker::{DummyChecker, DummyResult},
        types::{check_config::CheckConfig, result::CheckStatus},
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use uuid::Uuid;

    #[derive(Deserialize)]
    struct ErroredResult {
        details: Value,
        error: String,
    }

    #[tokio::test]
    async fn test_bad_request() {
        let checker = Arc::new(HttpChecker::DummyChecker(DummyChecker::new()));
        let app = new_router(checker.clone(), "region");

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/execute_config")
                    .header("Content-Type", "application/json")
                    .method("POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: ErroredResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.error, "serialization_error");

        // Test something that has part of a valid CheckConfig
        let req = json!({
            "subscription_id": Uuid::new_v4(),
        });
        let app = new_router(checker.clone(), "region");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/execute_config")
                    .header("Content-Type", "application/json")
                    .method("POST")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: ErroredResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.error, "serialization_error");
    }

    #[tokio::test]
    async fn test_bad_assertion() {
        let checker = Arc::new(HttpChecker::DummyChecker(DummyChecker::new()));
        let app = new_router(checker.clone(), "region");
        let config = CheckConfig {
            assertion: Some(Assertion {
                root: Op::JsonPath {
                    value: "%&@!#()".to_owned(),
                },
            }),
            ..Default::default()
        };

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/execute_config")
                    .header("Content-Type", "application/json")
                    .method("POST")
                    .body(Body::from(serde_json::to_vec(&config).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: ErroredResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.error, "assertion_compiler_error");
    }

    #[tokio::test]
    async fn test_success_result() {
        let success_result = DummyResult {
            delay: None,
            status: CheckStatus::Success,
        };
        let checker = DummyChecker::new();
        checker.queue_result(success_result);
        let checker = Arc::new(HttpChecker::DummyChecker(checker));
        let app = new_router(checker.clone(), "region");
        let config = CheckConfig::default();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/execute_config")
                    .header("Content-Type", "application/json")
                    .method("POST")
                    .body(Body::from(serde_json::to_vec(&config).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        // CheckResult has a &'static str in it, which is difficult to deserialize; just use json,
        // and check for a few values that we expect.
        let result: Value = serde_json::from_slice(&body).unwrap();
        let obj = result.as_object().unwrap();
        let cr = obj["check_result"].as_object().unwrap();
        assert_eq!(cr["region"], "region");
        assert_eq!(cr["status"], "success");
    }
}
