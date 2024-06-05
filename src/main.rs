// TODO: We might want to remove this once more stable, but it's just noisy for now.
#![allow(dead_code)]
mod checker;
mod cli;
mod cliapp;
mod scheduler;
mod types;

use std::process;

#[tokio::main]
pub async fn main() {
    let exit_code = match cli::execute().await {
        Ok(()) => 0,
        Err(_err) => {
            // TODO(epurkhiser): capture error? Here's what relay did
            // relay_log::ensure_error(&err);
            1
        }
    };
    process::exit(exit_code);
}
