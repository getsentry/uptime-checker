//! This module implements the definition of the command line app.

use std::path::PathBuf;

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
