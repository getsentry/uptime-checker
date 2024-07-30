use std::{io, sync::Arc};

pub mod cli;
pub mod config;

use clap::Parser;
use tokio::signal::ctrl_c;
use tracing::info;

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

    info!(config = ?config);

    match app.command {
        cli::Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let manager = Arc::new(Manager::new(config.clone()));

                let config_consumer_shutdown = manager.start();
                info!("Manager started");
                ctrl_c().await.expect("Failed to listen for ctrl-c signal");
                info!("Shutting down");
                manager.shutdown().await;
                // TODO: This should probably happen in the manager
                config_consumer_shutdown
                    .await
                    .expect("Failed waiting for config consumer shutdown");
                info!("Shut down");

                Ok(())
            }),
    }
}
