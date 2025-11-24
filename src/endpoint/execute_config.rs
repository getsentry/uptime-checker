use crate::endpoint::EndpointState;
use axum::{extract::State, response::IntoResponse, Json};
use http::StatusCode;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
#[serde(tag = "error")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecuteError {
    Unknown,
}

impl IntoResponse for ExecuteError {
    fn into_response(self) -> axum::response::Response {
        let status = match &self {
            ExecuteError::Unknown => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (status, Json(self)).into_response()
    }
}

pub(crate) async fn execute_config(
    State(_state): State<Arc<EndpointState>>,
) -> Result<(), ExecuteError> {
    Err(ExecuteError::Unknown)
}
