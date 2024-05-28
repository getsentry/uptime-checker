use std::io;

use crate::cliapp::make_app;

use tokio::signal::ctrl_c;

pub async fn execute() -> io::Result<()> {
    let app = make_app();
    let matches = app.get_matches();

    if let Some(_matches) = matches.subcommand_matches("run") {
        println!("Hello world. I am doing nothing. ^C to exit");
        ctrl_c().await
    } else {
        unreachable!();
    }
}
