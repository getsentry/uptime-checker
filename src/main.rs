#![deny(clippy::unwrap_used, clippy::panic)]
// TODO: We might want to remove this once more stable, but it's just noisy for now.
#![allow(dead_code)]
mod app;
mod assertions;
mod check_config_provider;
mod check_executor;
mod checker;
mod config_store;
mod config_waiter;
mod endpoint;
mod logging;
mod manager;
mod metrics;
mod producer;
mod redis;
mod scheduler;
mod test_utils;
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

#[cfg(test)]
mod test {
    use crate::app::config::Config;
    use redis::Client;

    #[ctor::dtor]
    fn cleanup() {
        // Flushes Redis back to the default state to avoid state crossover between runs.
        let config = Config::default();
        let client = Client::open(config.redis_host).unwrap();
        let mut conn = client.get_connection().unwrap();
        let _: () = redis::cmd("FLUSHDB").query(&mut conn).unwrap();
    }
}
