use std::io;

use tokio::signal::ctrl_c;

use crate::{
    cliapp::{Cli, Commands},
    logging,
    scheduler::run_scheduler,
};
use clap::Parser;

pub fn execute() -> io::Result<()> {
    let app = Cli::parse();

    logging::init();

    match app.command {
        Commands::Run => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                run_scheduler().await.expect("Failed to run scheduler");
                ctrl_c().await
            }),
    }
}
