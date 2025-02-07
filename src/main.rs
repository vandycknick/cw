mod aws;
mod commands;
mod config;
mod db;
mod editor;
mod logging;
mod proxy;
mod utils;

use crate::commands::Cw;
use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cw = Cw::parse();

    match cw.run() {
        Err(err) => {
            let root = err.root_cause();

            eprint!("\x1b[31m");
            eprintln!("Error: {}", err);
            eprintln!("");
            eprintln!("Caused by:");
            eprint!("  {}", root);
            eprintln!("\x1b[0m");
            ExitCode::from(1)
        }
        Ok(_) => ExitCode::from(0),
    }
}
