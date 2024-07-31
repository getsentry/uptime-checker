// TODO: We might want to remove this once more stable, but it's just noisy for now.
#![allow(dead_code)]
mod app;
mod checker;
mod config_consumer;
mod config_store;
mod config_waiter;
mod logging;
mod manager;
mod metrics;
mod producer;
mod scheduler;
mod types;

use std::process;

use sentry::Hub;

pub fn main() {
    let exit_code = match app::execute() {
        Ok(()) => 0,
        Err(_err) => {
            // TODO(epurkhiser): capture error? Here's what relay did
            // relay_log::ensure_error(&err);
            1
        }
    };

    Hub::current().client().map(|x| x.close(None));
    process::exit(exit_code);
}
