//! This module implements the definition of the command line app.

use std::path::PathBuf;

use crate::logging;
use clap::{Parser, Subcommand, ValueHint};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ABOUT: &str = "The Sentry uptime checker service.";

#[derive(Parser, Debug)]
#[clap(
    name = "uptime-checker",
    version = VERSION,
    about = ABOUT,
    disable_help_subcommand = true,
    subcommand_required = true,
)]
pub struct CliApp {
    #[clap(
        short,
        long,
        global = true,
        help = "The path to the config file.",
        value_hint = ValueHint::FilePath
    )]
    pub config: Option<PathBuf>,

    #[clap(
        short,
        long,
        global = true,
        value_name = "LEVEL",
        help = "Set the log level filter.",
        value_enum
    )]
    pub log_level: Option<logging::Level>,

    #[clap(
        long,
        global = true,
        value_name = "FORMAT",
        help = "Set the logging output format.",
        value_enum
    )]
    pub log_format: Option<logging::LogFormat>,

    #[clap(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Run the service
    #[clap(
        about = "Run the service",
        after_help = "This runs the uptime-checker in the foreground until it's shut down."
    )]
    Run,
}
