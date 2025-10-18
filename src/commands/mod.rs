use std::{fmt::Display, u8};

use clap::{command, Parser, Subcommand};
use eyre::Context;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, Layer};

use crate::{
    aws::LogClientBuilder,
    config::{ConfigManager, LocalConfigManager},
    db::{Database, Sqlite},
};

mod info;
mod list;
mod query;
mod tail;

#[derive(Subcommand, Debug)]
pub enum CwCmd {
    #[command(subcommand)]
    Ls(list::Cmd),

    Tail(tail::Cmd),

    Query(query::Cmd),

    Info(info::Cmd),
}

impl Display for CwCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CwCmd::Ls(cmd) => write!(f, "ls {}", cmd),
            CwCmd::Tail(_cmd) => write!(f, "tail"),
            CwCmd::Query(cmd) => write!(
                f,
                "query{}",
                cmd.command
                    .as_ref()
                    .map(|c| format!(" {}", c))
                    .unwrap_or_else(|| "".to_string())
            ),
            CwCmd::Info(_cmd) => write!(f, "info"),
        }
    }
}

#[derive(Parser)]
#[command(version)]
#[command(about = "Swiss army knife to query CloudWatch logs form the CLI.", long_about = None, disable_help_subcommand = true)]
pub struct Cw {
    #[arg(
        global = true,
        long,
        help = "The AWS profile to use. By default it will try to get the profile from the AWS_PROFILE environment variable.",
        display_order = 0
    )]
    pub profile: Option<String>,

    #[arg(
        global = true,
        long,
        help = "The AWS region to use. By default it will read this value from AWS_REGION env var or from the region set in the provided profile.",
        display_order = 0
    )]
    pub region: Option<String>,

    #[arg(global = true, long, help = "", display_order = 0)]
    pub endpoint: Option<String>,

    #[arg(
        long,
        short = 'v',
        action = clap::ArgAction::Count,
        global = true,
        help = "Write verbose messages to stderr for debugging.",
        display_order = 999
    )]
    pub verbose: u8,

    #[command(subcommand)]
    pub cmd: CwCmd,
}

impl Cw {
    fn log_filter(&self) -> LevelFilter {
        match self.verbose {
            0 => LevelFilter::OFF,
            1 => LevelFilter::ERROR,
            2 => LevelFilter::WARN,
            3 => LevelFilter::INFO,
            4 => LevelFilter::DEBUG,
            5 => LevelFilter::TRACE,
            6_u8..=u8::MAX => LevelFilter::TRACE,
        }
    }

    fn setup_logging(&self, config: &LocalConfigManager) -> eyre::Result<()> {
        let log_path = config
            .get_log_path()
            .context("Failed constructing file sink log path")?;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .context("Failed to open log file")?;

        let file_layer = fmt::Layer::default()
            .with_writer(file)
            .with_ansi(true)
            .with_target(true)
            .with_filter(self.log_filter());

        tracing_subscriber::registry()
            .with(file_layer)
            .try_init()
            .context("Failed setting up tracing subscriber")
    }

    pub fn run(self) -> eyre::Result<()> {
        let config = LocalConfigManager::new();
        self.setup_logging(&config)?;

        tracing::info!(target: "cw", "🐾 cw starting up!");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        tracing::info!(target: "cw", "running command {}", &self.cmd);
        tracing::trace!(target: "cw", "log level: {}", self.log_filter());

        let result = runtime.block_on(self.invoke_sub_command(config));

        if let Err(msg) = &result {
            tracing::error!(target: "cw", "failed running command {}, error={} cause={}", &self.cmd, msg, msg.root_cause());
            tracing::error!(target: "cw", "{:?}", msg);
        }

        result
    }

    async fn invoke_sub_command<T>(&self, config: T) -> eyre::Result<()>
    where
        T: ConfigManager,
    {
        let filter = self.log_filter();
        let client_builder = LogClientBuilder::new()
            .use_profile_name(self.profile.clone())
            .use_region(self.region.clone());

        let path = config.get_db_path()?;
        let db = Sqlite::new(&path).await?;

        if filter == LevelFilter::TRACE {
            let version = db.sqlite_version().await?;
            tracing::trace!(target: "cw", "SQLite Version: {}", version);
        }

        match &self.cmd {
            CwCmd::Ls(list) => list.run(&client_builder).await,
            CwCmd::Tail(tail) => tail.run(&client_builder).await,
            CwCmd::Query(query) => query.run(&client_builder, db).await,
            CwCmd::Info(info) => info.run(&config, db).await,
        }
    }
}
