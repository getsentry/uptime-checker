use std::io;

use tokio::signal::ctrl_c;

use crate::{cliapp::make_app, logging, scheduler::run_scheduler};

pub fn execute() -> io::Result<()> {
    let app = make_app();
    let matches = app.get_matches();

    logging::init();

    if let Some(_matches) = matches.subcommand_matches("run") {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                run_scheduler().await.expect("Failed to run scheduler");
                ctrl_c().await
            })
    } else {
        unreachable!();
    }
}
