mod args;
mod config;
mod cw;
mod db;
mod editor;

use std::process::ExitCode;

use clap::Parser;

use crate::args::GlobalArgs;
use crate::cw::Cw;

async fn run() -> eyre::Result<()> {
    let args = GlobalArgs::parse();
    let cw = Cw::new(args).await;
    cw.run().await?;
    Ok(())
}

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();

    if let Err(err) = runtime {
        eprint!("\x1b[31m");
        eprint!("{}", err);
        eprintln!("\x1b[0m");

        return ExitCode::from(2);
    }

    match runtime.unwrap().block_on(run()) {
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
