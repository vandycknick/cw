use clap::{Args, CommandFactory};

use crate::{commands::Cw, config::ConfigManager, db::Database};

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cmd {}

impl Cmd {
    pub async fn run(&self, config: &impl ConfigManager, db: impl Database) -> eyre::Result<()> {
        let version = db.version().await?;
        let engine = db.engine();

        println!(
            "Version:        {}",
            Cw::command().get_version().unwrap_or("")
        );
        println!("Database:       {}-{}", engine, version);
        println!(
            "Database Path:  {}",
            config.get_db_path().unwrap_or("".to_string())
        );
        println!(
            "Logs:           {}",
            config.get_log_path().unwrap_or("".to_string())
        );
        Ok(())
    }
}
