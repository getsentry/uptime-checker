mod cli;
mod cliapp;

use std::process;

pub fn main() {
    let exit_code = match cli::execute() {
        Ok(()) => 0,
        Err(_err) => {
            // TODO(epurkhiser): capture error? Here's what relay did
            // relay_log::ensure_error(&err);
            1
        }
    };
    process::exit(exit_code);
}
