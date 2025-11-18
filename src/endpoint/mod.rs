mod execute_config;

use crate::endpoint::execute_config::execute_config;
use crate::{app::config::Config, checker::HttpChecker};
use axum::{routing::get, Router};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct EndpointState {
    checker: Arc<HttpChecker>,
}

pub fn start_endpoint(
    config: &Arc<Config>,
    shutdown_signal: CancellationToken,
    checker: Arc<HttpChecker>,
) -> JoinHandle<()> {
    let app = Router::new()
        .route("/execute_config", get(execute_config))
        .with_state(Arc::new(EndpointState { checker }));

    let endpoint_port = config.webserver_port;

    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(format!("localhost:{}", endpoint_port))
            .await
            .unwrap();
        let shutdown_fut = shutdown_signal.cancelled_owned();
        let f = axum::serve(listener, app).with_graceful_shutdown(shutdown_fut);
        f.await.unwrap();
    })
}
