use std::io;

use clap::Parser;
use tokio::signal::ctrl_c;

use crate::{
    cliapp::{CliApp, Commands},
    config::Config,
    logging::{self, LoggingConfig},
    scheduler::run_scheduler,
};

pub fn execute() -> io::Result<()> {
    let app = CliApp::parse();
    let config = Config::extract(&app.config).expect("Configuration invalid");

    logging::init(LoggingConfig::from_config(&config));

    match app.command {
        Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                run_scheduler(&config)
                    .await
                    .expect("Failed to run scheduler");
                ctrl_c().await
            }),
    }
}
