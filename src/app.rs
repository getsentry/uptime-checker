use std::{io, sync::Arc, thread};

pub mod cli;
pub mod config;

use clap::Parser;
use tokio::signal::ctrl_c;

use crate::{
    logging::{self, LoggingConfig},
    manager::Manager,
    metrics,
};

pub fn execute() -> io::Result<()> {
    let app = cli::CliApp::parse();
    let config = Arc::new(config::Config::extract(&app).expect("Configuration invalid"));

    logging::init(LoggingConfig::from_config(&config));
    metrics::init(&config.metrics);

    tracing::info!(config = ?config);

    let num_cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    match app.command {
        cli::Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(num_cpus * config.thread_cpu_scale_factor)
            .build()
            .unwrap()
            .block_on(async {
                let shutdown = Manager::start(config);
                tracing::info!("system.manager_started");

                ctrl_c().await.expect("Failed to listen for SIGINT signal");

                tracing::info!("system.got_sigint");
                shutdown().await;
                tracing::info!("system.shutdown");

                Ok(())
            }),
    }
}
