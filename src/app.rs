use std::{io, sync::Arc};

pub mod cli;
pub mod config;

use chrono::TimeDelta;
use clap::Parser;
use tokio::{join, signal::ctrl_c};
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::uuid;

use crate::{
    config_consumer::run_config_consumer,
    config_store::ConfigStore,
    logging::{self, LoggingConfig},
    metrics::{self, MetricsConfig},
    scheduler::run_scheduler,
    types::check_config::{CheckConfig, CheckInterval},
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

                // XXX: Example config while we build out the consumer that loads configs
                config_store
                    .write()
                    .expect("Lock poisoned")
                    .add_config(Arc::new(CheckConfig {
                        partition: 0,
                        url: "https://downtime-simulator-test1.vercel.app".to_string(),
                        subscription_id: uuid!("663399a09e6340a79c3c7a3f26878904"),
                        interval: CheckInterval::FiveMinutes,
                        timeout: TimeDelta::seconds(5),
                    }));

                let shutdown_signal = CancellationToken::new();

                let config_consumer =
                    run_config_consumer(&config, config_store.clone(), shutdown_signal.clone());

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
