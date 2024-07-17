use std::{io, sync::Arc};

pub mod cli;
pub mod config;

use clap::Parser;
use tokio::{join, signal::ctrl_c};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{
    config_consumer::run_config_consumer,
    config_store::ConfigStore,
    logging::{self, LoggingConfig},
    metrics::{self, MetricsConfig},
    scheduler::run_scheduler,
};

pub fn execute() -> io::Result<()> {
    let app = cli::CliApp::parse();
    let config = config::Config::extract(&app).expect("Configuration invalid");

    logging::init(LoggingConfig::from_config(&config));
    metrics::init(MetricsConfig::from_config(&config));

    info!(config = ?config);

    match app.command {
        cli::Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let config_store = Arc::new(ConfigStore::new_rw());

                let shutdown_signal = CancellationToken::new();

                let (config_consumer, consumer_booting) =
                    run_config_consumer(&config, config_store.clone(), shutdown_signal.clone());

                // Wait for the config consumre to read the backlog of configs before continuing to
                // start the scheduler. We don't want to start scheduling until we've read all the
                // configs since we want to avoid scheduling old configs.
                consumer_booting
                    .await
                    .expect("Failed to wait for consumer to boot");

                let check_scheduler =
                    run_scheduler(&config, config_store.clone(), shutdown_signal.clone());

                ctrl_c().await.expect("Failed to listen for ctrl-c signal");
                shutdown_signal.cancel();

                // TODO(epurkhiser): Do we need to be concerned about the error results here?
                let _ = join!(config_consumer, check_scheduler);

                Ok(())
            }),
    }
}
