use std::io;

use tokio::signal::ctrl_c;

use crate::{cliapp::make_app, scheduler::run_scheduler};

pub async fn execute() -> io::Result<()> {
    let app = make_app();
    let matches = app.get_matches();

    if let Some(_matches) = matches.subcommand_matches("run") {
        run_scheduler().await.expect("Failed to run scheduler");
        ctrl_c().await
    } else {
        unreachable!();
    }
}
