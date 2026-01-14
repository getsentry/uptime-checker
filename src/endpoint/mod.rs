mod execute_config;
mod validate_check;

use crate::endpoint::execute_config::execute_config;
use crate::endpoint::validate_check::validate_check;
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
        .route("/validate_check", post(validate_check))
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
        let listener = tokio::net::TcpListener::bind(format!("localhost:{endpoint_port}")).await;

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
