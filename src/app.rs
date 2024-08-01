use std::{io, sync::Arc};

pub mod cli;
pub mod config;

use clap::Parser;
use tokio::signal::ctrl_c;

use crate::{
    logging::{self, LoggingConfig},
    manager::Manager,
    metrics::{self, MetricsConfig},
};

pub fn execute() -> io::Result<()> {
    let app = cli::CliApp::parse();
    let config = Arc::new(config::Config::extract(&app).expect("Configuration invalid"));

    logging::init(LoggingConfig::from_config(&config));
    metrics::init(MetricsConfig::from_config(&config));

    tracing::info!(config = ?config);

    match app.command {
        cli::Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let manager = Arc::new(Manager::new(config.clone()));

                let shutdown = manager.start();
                tracing::info!("system.manager_started");

                ctrl_c().await.expect("Failed to listen for SIGINT signal");

                tracing::info!("system.got_sigint");
                shutdown().await;
                tracing::info!("system.shutdown");

                Ok(())
            }),
    }
}
