use std::fmt::Display;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use aws_sdk_cloudwatchlogs::types::QueryStatus;
use chrono::Utc;
use clap::{Args, Subcommand};
use eyre::Context;
use serde_json::{Map, Value};
use tokio::fs::File;
use tokio::io::AsyncReadExt;
use tokio::time::sleep;

use crate::commands::LogClientBuilder;
use crate::db::{Database, QueryHistory};
use crate::editor::open_in_editor;
use crate::utils::parse_human_time;

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Cmd {
    #[arg(index = 1, value_name = "file_or_query_name")]
    pub file_or_query_name: Option<String>,

    #[arg(short, long, required = true)]
    pub group_names: Vec<String>,

    #[arg(short, long, value_parser = parse_human_time)]
    pub start_time: Option<i64>,

    #[arg(short, long, value_parser = parse_human_time)]
    pub end_time: Option<i64>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    History,
}

impl Display for Commands {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Commands::History => write!(f, "history"),
        }
    }
}

impl Cmd {
    pub async fn run(&self, builder: &LogClientBuilder, db: impl Database) -> eyre::Result<()> {
        match &self.command {
            None => self.run_query(builder, db).await,
            Some(cmd) => self.run_command(cmd, db).await,
        }
    }

    pub async fn get_query_from_file_or_query_name(
        &self,
        file_or_query_name: &str,
    ) -> eyre::Result<String> {
        // FIX: for now fail until stored queries are implemented.
        let path = PathBuf::from_str(file_or_query_name)?;

        if !path.exists() {
            return Err(eyre::eyre!("File provided via -file does not exist!"));
        }

        let mut file = File::open(path).await?;
        let mut query = String::new();
        file.read_to_string(&mut query).await?;
        Ok(query)
    }

    pub async fn run_query(
        &self,
        builder: &LogClientBuilder,
        db: impl Database,
    ) -> eyre::Result<()> {
        let client = builder.build().await?;
        let query = if let Some(file_or_query_name) = &self.file_or_query_name {
            self.get_query_from_file_or_query_name(file_or_query_name)
                .await?
        } else {
            let sample = "# vim: ft=lq\n";
            let query = open_in_editor(sample, None)?;

            query
                .strip_prefix(sample)
                .unwrap_or(query.as_str())
                .to_string()
        };

        let query_result = client
            .start_query()
            .set_log_group_names(Some(self.group_names.clone()))
            .query_string(&query)
            .start_time(
                self.start_time
                    // TODO: set start to 1h ago by default
                    .unwrap_or_else(|| (Utc::now().timestamp() - 30) * 1000),
            )
            .end_time(
                self.end_time
                    .unwrap_or_else(|| Utc::now().timestamp() * 1000),
            )
            .send()
            .await
            .context("Failed creating AWS CW Query Client.")?;

        let Some(query_id) = query_result.query_id() else {
            return Err(eyre::eyre!("File provided via -file does not exist!"));
        };

        tracing::info!("Collecting events for query with id {}", query_id);
        let mut history = QueryHistory::new(query_id.to_string(), query);
        db.save(&history).await?;

        loop {
            let output = client.get_query_results().query_id(query_id).send().await?;

            match output.status {
                Some(QueryStatus::Scheduled) => {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Some(QueryStatus::Running) => {
                    history.set_status(crate::db::QueryStatus::Running);
                    db.update(&history).await?;
                    sleep(Duration::from_secs(2)).await;
                    continue;
                }
                Some(QueryStatus::Complete) => {
                    let statistics = output.statistics().unwrap();
                    let results = output.results();

                    history.set_status(crate::db::QueryStatus::Complete);
                    history.set_statistics(
                        results.len() as i64,
                        statistics.records_matched,
                        statistics.records_scanned,
                        statistics.bytes_scanned,
                    );
                    db.update(&history).await?;

                    tracing::info!("[{}] status: {}.", query_id, history.status);
                    tracing::info!(
                        "[{}] showing: {} of {} records matched.",
                        query_id,
                        history.records_total,
                        history.records_matched
                    );

                    let duration = history.modified_at - history.created_at;
                    tracing::info!(
                        "[{}] {} records ({} bytes) scanned in {},{}s.",
                        query_id,
                        history.records_scanned,
                        history.bytes_scanned,
                        duration.num_seconds(),
                        duration.num_milliseconds() - (duration.num_seconds() * 1000)
                    );

                    for line in results {
                        let mut json = Map::new();
                        for record in line {
                            if let Some(field) = record.field() {
                                // NOTE: Expose a flag wether to log the ptr or not.
                                if field == "@ptr" {
                                    continue;
                                }

                                json.insert(
                                    field.to_string(),
                                    Value::String(record.value().unwrap_or("").to_string()),
                                );
                            }
                        }
                        println!("{}", serde_json::to_string(&json)?);
                    }
                    break;
                }
                Some(QueryStatus::Failed) => {
                    history.set_status(crate::db::QueryStatus::Failed);
                    db.update(&history).await?;
                    return Err(eyre::eyre!("Query failed: {}", history.query_id));
                }
                Some(QueryStatus::Timeout) => {
                    history.set_status(crate::db::QueryStatus::Timeout);
                    db.update(&history).await?;
                    return Err(eyre::eyre!("Query timed out: {}", history.query_id));
                }
                None => {
                    tracing::info!(
                        "[{}] No status returned, unsure if I should proceed, exiting for now",
                        query_id
                    );
                    break;
                }
                _ => {
                    tracing::error!("[{}] UNHANDLED status: {:?}", query_id, output.status);
                    break;
                }
            }
        }

        Ok(())
    }

    pub async fn run_command(&self, cmd: &Commands, db: impl Database) -> eyre::Result<()> {
        match cmd {
            Commands::History => {
                for item in db.list().await? {
                    println!("{} | {}", item.query_id, item.contents);
                }
                Ok(())
            }
        }
    }
}
