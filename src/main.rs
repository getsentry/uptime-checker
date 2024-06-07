// TODO: We might want to remove this once more stable, but it's just noisy for now.
#![allow(dead_code)]
mod checker;
mod cli;
mod cliapp;
mod logging;
mod scheduler;
mod types;
mod producer;

use std::process;

use sentry::Hub;

pub fn main() {
    let exit_code = match cli::execute() {
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
