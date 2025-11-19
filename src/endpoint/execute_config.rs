use crate::{
    assertions::compiled::{self, compile},
    check_executor::{do_check, CheckSender, ScheduledCheck},
    config_store::Tick,
    endpoint::EndpointState,
    producer::{ExtractCodeError, ResultsProducer},
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

pub struct ResultProducer {}

impl ResultProducer {
    pub fn new() -> Self {
        Self {}
    }
}

impl ResultsProducer for ResultProducer {
    fn produce_checker_result(&self, _: &CheckResult) -> Result<(), ExtractCodeError> {
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "error")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecuteError {
    SerializationError { details: String },
    CheckerShutdown,
    AssertionCompilerError { details: compiled::Error },
}

impl IntoResponse for ExecuteError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            ExecuteError::SerializationError { details: _ } => StatusCode::BAD_REQUEST,
            ExecuteError::CheckerShutdown => StatusCode::INTERNAL_SERVER_ERROR,
            ExecuteError::AssertionCompilerError { details: _ } => StatusCode::BAD_REQUEST,
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
    AppJson(mut check_config): AppJson<CheckConfig>,
) -> Result<AppJson<ResultWrapper>, ExecuteError> {
    if let Some(assertion) = &check_config.assertion {
        check_config.compiled_assertion = Some(
            compile(assertion).map_err(|e| ExecuteError::AssertionCompilerError { details: e })?,
        );
    };

    let (sender, _) = CheckSender::new();

    let (resolve_tx, resolve_rx) = oneshot::channel();
    let sc = ScheduledCheck::new(
        crate::check_executor::CheckKind::Uptime,
        Tick::from_time(Utc::now()),
        Arc::new(check_config),
        resolve_tx,
    );
    let producer = Arc::new(ResultProducer::new());

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
