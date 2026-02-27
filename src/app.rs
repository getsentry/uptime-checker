use std::{io, sync::Arc, thread};

pub mod cli;
pub mod config;

use clap::Parser;
use tokio::{select, signal::ctrl_c};

use crate::{
    logging::{self, LoggingConfig},
    manager::Manager,
    metrics,
};

pub fn execute() -> io::Result<()> {
    let app = cli::CliApp::parse();
    let config = Arc::new(config::Config::extract(&app).expect("Config should be valid"));

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
            .expect("Tokio runtime should be able to start up")
            .block_on(async {
                let mut shutdown = Manager::start(config);
                tracing::info!("system.manager_started");

                let sigint = ctrl_c();
                select! {
                    _ = sigint => {
                        tracing::info!("system.got_sigint");
                    }
                    result = shutdown.recv_task_finished() => {
                        match result {
                            None => tracing::info!("system.recv_task_finished.channel_closed"),
                            Some(Err(err)) => {
                                tracing::error!(%err, "system.recv_task_finished.shutdown_error");
                            },
                            Some(Ok(_)) => tracing::info!("system.recv_task_finished.end_of_task"),
                        }
                    }
                }
                if let Err(err) = shutdown.stop().await {
                    tracing::error!(%err, "system.shutdown_error");
                }
                tracing::info!("system.shutdown");
                Ok(())
            }),
    }
}
