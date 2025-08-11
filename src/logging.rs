use std::{borrow::Cow, str::FromStr};

use sentry::{integrations::tracing::EventFilter, types::Dsn};
use serde::{Deserialize, Serialize};
use tracing::level_filters::LevelFilter;

use crate::app::{cli::VERSION, config};

use tracing_subscriber::{prelude::*, Layer};

/// Controls the log format.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Auto detect the best format.
    Auto,

    /// Pretty printing with colors.
    Pretty,

    /// Simplified plain text output.
    Simplified,

    /// Dump out JSON lines.
    Json,
}

/// The logging level
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    Off,
}

impl Level {
    /// Returns the tracing [`LevelFilter`].
    pub const fn level_filter(&self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::ERROR,
            Self::Warn => LevelFilter::WARN,
            Self::Info => LevelFilter::INFO,
            Self::Debug => LevelFilter::DEBUG,
            Self::Trace => LevelFilter::TRACE,
            Self::Off => LevelFilter::OFF,
        }
    }
}

#[derive(Debug)]
pub struct LoggingConfig {
    /// The DSN key to use for sentry error reporting. If left empty sentry will not be
    /// initialized.
    pub sentry_dsn: Option<String>,

    /// The sentry environment to report errors to.
    pub sentry_env: Option<Cow<'static, str>>,

    /// The configured log level
    pub log_level: Level,

    /// The logging format to output
    pub log_format: LogFormat,
}

impl LoggingConfig {
    pub fn from_config(config: &config::Config) -> Self {
        Self {
            sentry_dsn: config.sentry_dsn.to_owned(),
            sentry_env: config.sentry_env.to_owned(),
            log_level: config.log_level,
            log_format: config.log_format,
        }
    }
}

pub fn init(config: LoggingConfig) {
    if let Some(dsn) = &config.sentry_dsn {
        let dsn = Some(Dsn::from_str(dsn).expect("DSN should be valid"));

        let guard = sentry::init(sentry::ClientOptions {
            dsn,
            release: Some(Cow::Borrowed(VERSION)),
            environment: config.sentry_env.to_owned(),
            ..Default::default()
        });

        // We manually deinitialize sentry later
        std::mem::forget(guard)
    }

    let subscriber = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true);

    let format = match (config.log_format, console::user_attended()) {
        (LogFormat::Auto, true) | (LogFormat::Pretty, _) => {
            subscriber.compact().without_time().boxed()
        }
        (LogFormat::Auto, false) | (LogFormat::Simplified, _) => {
            subscriber.with_ansi(false).boxed()
        }
        (LogFormat::Json, _) => subscriber
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_file(true)
            .with_line_number(true)
            .boxed(),
    };

    // Same as the default filter, except it sends everything at or above INFO as logs instead of breadcrumbs.
    let sentry_layer =
        sentry::integrations::tracing::layer().event_filter(|md| match *md.level() {
            tracing::Level::ERROR => EventFilter::Event | EventFilter::Log,
            tracing::Level::WARN | tracing::Level::INFO => EventFilter::Log,
            tracing::Level::DEBUG | tracing::Level::TRACE => EventFilter::Ignore,
        });

    let logs_subscriber = tracing_subscriber::registry()
        .with(format.with_filter(config.log_level.level_filter()))
        .with(sentry_layer);

    logs_subscriber.init();
}
