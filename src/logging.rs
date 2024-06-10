use std::{borrow::Cow, str::FromStr};

use sentry::types::Dsn;

use crate::config::Config;

#[derive(Debug)]
pub struct LoggingConfig {
    /// The DSN key to use for sentry error reporting. If left empty sentry will not be
    /// initialized.
    pub sentry_dsn: Option<String>,

    /// The sentry environment to report errors to.
    pub sentry_env: Option<Cow<'static, str>>,
}

impl LoggingConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            sentry_dsn: config.sentry_dsn.to_owned(),
            sentry_env: config.sentry_env.to_owned(),
        }
    }
}

pub fn init(config: LoggingConfig) {
    if let Some(dsn) = &config.sentry_dsn {
        let dsn = Some(Dsn::from_str(dsn).expect("Invalid Sentry DSN"));

        let guard = sentry::init(sentry::ClientOptions {
            dsn,
            release: sentry::release_name!(),
            environment: config.sentry_env.to_owned(),
            ..Default::default()
        });

        // We manually deinitalize sentry later
        std::mem::forget(guard)
    }
}
