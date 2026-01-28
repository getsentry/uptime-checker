use crate::{
    check_executor::{do_check, CheckSender, ScheduledCheck},
    config_store::Tick,
    endpoint::EndpointState,
    producer::noop_producer::NoopResultsProducer,
    types::{check_config::CheckConfig, result::CheckResult},
};
use axum::{
    extract::{rejection::JsonRejection, FromRequest, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use http::StatusCode;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::oneshot;

#[derive(Debug, Serialize)]
#[serde(tag = "error")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecuteError {
    SerializationError { details: String },
    CheckerShutdown,
}

impl IntoResponse for ExecuteError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            ExecuteError::SerializationError { details: _ } => StatusCode::BAD_REQUEST,
            ExecuteError::CheckerShutdown => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (status, Json(self)).into_response()
    }
}

impl From<JsonRejection> for ExecuteError {
    fn from(rejection: JsonRejection) -> Self {
        Self::SerializationError {
            details: rejection.body_text(),
        }
    }
}

#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ExecuteError))]
pub(crate) struct AppJson<T>(T);

impl<T> IntoResponse for AppJson<T>
where
    axum::Json<T>: IntoResponse,
{
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ResultWrapper {
    pub(crate) check_result: Option<CheckResult>,
}

pub(crate) async fn execute_config(
    State(state): State<Arc<EndpointState>>,
    AppJson(check_config): AppJson<CheckConfig>,
) -> Result<AppJson<ResultWrapper>, ExecuteError> {
    let (sender, _) = CheckSender::new();

    let (resolve_tx, resolve_rx) = oneshot::channel();
    let sc = ScheduledCheck::new(
        crate::check_executor::CheckKind::Uptime,
        Tick::from_time(Utc::now()),
        Arc::new(check_config),
        true,
        resolve_tx,
    );

    // We aren't going to produce the result to anything; we'll use the resolve_rx channel
    // to await the CheckResult here in the endpoint.
    let producer = Arc::new(NoopResultsProducer::new());

    do_check(
        0,
        sc,
        state.checker.clone(),
        Arc::new(sender),
        producer,
        state.region,
    )
    .await;

    let Ok(check_result) = resolve_rx.await else {
        return Err(ExecuteError::CheckerShutdown);
    };

    Ok(AppJson(ResultWrapper { check_result }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        checker::{
            dummy_checker::{DummyChecker, DummyResult},
            HttpChecker,
        },
        endpoint::new_router,
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
