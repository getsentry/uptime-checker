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
