use crate::cliapp::make_app;
use anyhow::Result;

pub fn execute() -> Result<()> {
    let app = make_app();
    let matches = app.get_matches();

    if let Some(_matches) = matches.subcommand_matches("run") {
        println!("Hello world");
        Ok(())
    } else {
        unreachable!();
    }
}
