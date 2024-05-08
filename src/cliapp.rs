//! This module implements the definition of the command line app.

use clap::builder::ValueParser;
use clap::{Arg, Command, ValueHint};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ABOUT: &str = "The Sentry uptime checker service.";

pub fn make_app() -> Command {
    Command::new("relay")
        .disable_help_subcommand(true)
        .subcommand_required(true)
        .propagate_version(true)
        .version(VERSION)
        .about(ABOUT)
        .arg(
            Arg::new("config")
                .long("config")
                .short('c')
                .global(true)
                .value_hint(ValueHint::DirPath)
                .value_parser(ValueParser::path_buf())
                .help("The path to the config file."),
        )
        .subcommand(
            Command::new("run")
                .about("Run the service")
                .after_help("This runs the uptime-checker in the foreground until it's shut down."),
        )
}
