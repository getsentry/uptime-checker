use crate::{assertions::compiled::compile, types::check_config::CheckConfig};
use axum::{
    extract::{rejection::JsonRejection, FromRequest},
    response::IntoResponse,
    Json,
};

use http::StatusCode;
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(tag = "error")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecuteError {
    SerializationError { details: String },
    CompilationError { details: String },
}

impl IntoResponse for ExecuteError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            ExecuteError::SerializationError { details: _ } => StatusCode::BAD_REQUEST,
            ExecuteError::CompilationError { details: _ } => StatusCode::BAD_REQUEST,
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

pub(crate) async fn validate_check(
    AppJson(check_config): AppJson<CheckConfig>,
) -> Result<(), ExecuteError> {
    if let Some(assertion) = check_config.assertion {
        if let Err(err) = compile(&assertion) {
            return Err(ExecuteError::CompilationError {
                details: err.to_string(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        assertions::{Assertion, GlobPattern, Op},
        checker::{dummy_checker::DummyChecker, HttpChecker},
        endpoint::new_router,
        types::check_config::CheckConfig,
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use chrono::TimeDelta;
    use http_body_util::BodyExt;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::sync::Arc;
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
                    .uri("/validate_check")
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
                    .uri("/validate_check")
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
    async fn test_compilation_error() {
        let checker = Arc::new(HttpChecker::DummyChecker(DummyChecker::new()));
        let valid_check = CheckConfig {
            subscription_id: Uuid::new_v4(),
            interval: crate::types::check_config::CheckInterval::FiveMinutes,
            timeout: TimeDelta::min_value(),
            url: "https://foo.bar".to_owned(),
            request_method: crate::types::shared::RequestMethod::Get,
            request_headers: vec![],
            request_body: String::new(),
            trace_sampling: false,
            active_regions: None,
            region_schedule_mode: None,
            assertion: Some(Assertion {
                root: Op::HeaderCheck {
                    key: crate::assertions::HeaderComparison::Equals {
                        test_value: crate::assertions::HeaderOperand::Glob {
                            pattern: GlobPattern {
                                // invalid glob pattern
                                value: "[]]])(".to_owned(),
                            },
                        },
                    },
                    value: crate::assertions::HeaderComparison::Always,
                },
            }),
        };

        let app = new_router(checker.clone(), "region");
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/validate_check")
                    .header("Content-Type", "application/json")
                    .method("POST")
                    .body(Body::from(serde_json::to_vec(&valid_check).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let result: ErroredResult = serde_json::from_slice(&body).unwrap();
        assert_eq!(result.error, "compilation_error");
    }
}
