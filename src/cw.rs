use eyre::{eyre, Context, OptionExt};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;

use aws_config::retry::RetryConfig;
use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use aws_sdk_cloudwatchlogs::types::{FilteredLogEvent, QueryStatus};

use aws_sdk_cloudwatchlogs::{types, Client};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use chronoutil::RelativeDuration;
use serde_json::{json, Map, Value};

use crate::args::{Command, GlobalArgs, LsCommand, QueryArgs, TailArgs};
use crate::config::{ConfigManager, LocalConfigManager};
use crate::db::{Database, QueryHistory, Sqlite};
use crate::editor::open_in_editor;

pub async fn create_log_client(profile_name: Option<impl Into<String>>) -> cloudwatchlogs::Client {
    let mut builder = aws_config::from_env().retry_config(RetryConfig::standard());
    if let Some(profile_name) = profile_name {
        builder = builder.profile_name(profile_name);
    }

    let config = builder.load().await;

    let client = cloudwatchlogs::Client::new(&config);
    client
}

pub struct Cw {
    args: GlobalArgs,
    client: Client,
}

impl Cw {
    pub async fn new(args: GlobalArgs) -> Self {
        let client = create_log_client(args.profile.clone()).await;
        Self { args, client }
    }

    pub async fn run(&self) -> eyre::Result<()> {
        match &self.args.cmd {
            Command::Ls(LsCommand::Groups) => self.list_groups().await,
            Command::Ls(LsCommand::Streams { group_name }) => self.list_streams(group_name).await,
            Command::Tail(args) => self.tail(args).await,
            Command::Query(args) => {
                let config = LocalConfigManager::new();
                let path = config.get_db_path()?;
                let db = Sqlite::new(&path).await?;
                self.query(args, db).await
            }
        }
    }

    pub async fn list_groups(&self) -> eyre::Result<()> {
        let client = &self.client;

        let mut next_token: Option<String> = None;

        loop {
            let mut builder = client
                .describe_log_groups()
                // NOTE: 50 is the maximum, ref: https://docs.aws.amazon.com/AmazonCloudWatchLogs/latest/APIReference/API_DescribeLogGroups.html#CWL-DescribeLogGroups-request-limit
                .limit(50);

            if let Some(ref token) = next_token {
                builder = builder.next_token(token);
            }

            let response = builder
                .send()
                .await
                .wrap_err("Failed creating AWS Client.")?;
            let groups = response.log_groups();

            for group in groups {
                println!("{}", group.log_group_name().unwrap_or_default());
            }

            next_token = response.next_token().map(|t| t.to_string());

            if next_token == None {
                break;
            }
        }
        Ok(())
    }

    pub async fn list_streams(&self, group_name: impl Into<String>) -> eyre::Result<()> {
        let mut next_token: Option<String> = None;
        let group_name = group_name.into();

        loop {
            let mut builder = self
                .client
                .describe_log_streams()
                .log_group_identifier(&group_name)
                .order_by(types::OrderBy::LastEventTime)
                .descending(true)
                // NOTE: 50 is the maximum, ref:
                .limit(50);

            if let Some(ref token) = next_token {
                builder = builder.next_token(token);
            }

            let six_months = RelativeDuration::months(6);
            let sm_ago = Utc::now() - six_months;

            let response = builder
                .send()
                .await
                .wrap_err("Failed creating AWS Client.")?;

            let streams =
                response
                    .log_streams()
                    .iter()
                    .filter(|s| match s.last_event_timestamp() {
                        Some(timestamp) => {
                            // TODO: check if default is actually creating the correct behaviour here
                            DateTime::from_timestamp_millis(timestamp).unwrap_or_default() > sm_ago
                        }
                        None => false,
                    });

            for stream in streams {
                println!("{}", stream.log_stream_name().unwrap_or_default());
            }

            next_token = response.next_token().map(|t| t.to_string());

            if next_token == None {
                break;
            }
        }
        Ok(())
    }

    pub async fn tail(&self, args: &TailArgs) -> eyre::Result<()> {
        let (group_name, stream_name) = args.parse_group_and_stream();

        let group_name = group_name.ok_or_eyre(format!(
            "Invalid group or stream name, group name can't be empty: {}",
            args.group_and_stream_prefix
        ))?;

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        let start_time = args
            .start_time
            // NOTE: Moving `now` slightly into the past. That way it's more
            // likely that an empty start time atleast returns something.
            .unwrap_or_else(|| (Utc::now().timestamp() - 30) * 1000);

        let log_producer = tokio::spawn(Self::tail_log_producer(
            self.client.clone(),
            sender,
            start_time,
            args.filter.clone(),
            args.follow,
            group_name.clone(),
            stream_name,
        ));

        let log_writer = tokio::spawn(Self::write_log_event(
            receiver,
            args.output.clone(),
            group_name.clone(),
            args.local,
            args.timestamp,
            args.group_name,
            args.stream_name,
            args.event_id,
        ));

        log_producer.await??;
        log_writer.await??;

        Ok(())
    }

    async fn tail_log_producer(
        client: Client,
        sender: UnboundedSender<FilteredLogEvent>,
        start_time: i64,
        filter: Option<String>,
        follow: bool,
        group_name: String,
        stream_name: Option<String>,
    ) -> eyre::Result<()> {
        let mut start_time = start_time;
        let mut next_token: Option<String> = None;
        let mut builder = client
            .filter_log_events()
            .log_group_name(&group_name)
            .start_time(start_time)
            .limit(10_000);
        if let Some(stream_name) = &stream_name {
            builder = builder.log_stream_name_prefix(stream_name);
        }

        if let Some(filter_pattern) = &filter {
            builder = builder.filter_pattern(filter_pattern);
        }

        loop {
            builder = builder.start_time(start_time);
            builder = builder.set_next_token(next_token.clone());
            let response = builder
                .clone()
                .send()
                .await
                .wrap_err_with(|| "Can't create AWS client")?;

            if response.events.is_none() {
                continue;
            }

            if let Some(events) = &response.events {
                for event in events {
                    // NOTE: This will cause the program to exit from the moment I can't
                    // send any events anymore. I'm unsure if I really want this. But I prefer
                    // that over ignoring for now.
                    sender.send(event.clone())?;
                }
            }

            next_token = response.next_token().map(|s| s.to_string());

            if next_token == None && !follow {
                break;
            }

            if let Some(last_event) = &response.events().last() {
                start_time = last_event.timestamp().unwrap() + 1;
            }
        }
        Ok(())
    }

    async fn write_log_event(
        receiver: UnboundedReceiver<FilteredLogEvent>,
        output_type: crate::args::OutputType,
        group_name: String,
        use_local_time: bool,
        write_timestamp: bool,
        write_group_name: bool,
        write_stream_name: bool,
        write_event_id: bool,
    ) -> eyre::Result<()> {
        let mut writer = tokio::io::stdout();
        let use_colors = std::io::stdout().is_terminal();
        let mut receiver = receiver;
        let reset_ansi = if use_colors { "\x1b[0m" } else { "" };

        while let Some(event) = receiver.recv().await {
            match output_type {
                crate::args::OutputType::Text => {
                    let mut line: String = "".to_owned();

                    if write_timestamp {
                        if let Some(time) = event
                            .timestamp()
                            .and_then(|ts| parse_timestamp(ts, use_local_time))
                        {
                            let color = if use_colors { "\x1b[32m" } else { "" };
                            line.push_str(format!("{}{} {}- ", color, time, reset_ansi).as_str());
                        }
                    }

                    if write_group_name {
                        let color = if use_colors { "\x1b[34m" } else { "" };
                        line.push_str(format!("{}{} {}- ", color, group_name, reset_ansi).as_str());
                    }

                    if write_stream_name && event.log_stream_name.is_some() {
                        let color = if use_colors { "\x1b[36m" } else { "" };
                        line.push_str(
                            format!(
                                "{}{} {}- ",
                                color,
                                event.log_stream_name().unwrap(),
                                reset_ansi
                            )
                            .as_str(),
                        );
                    }

                    if write_event_id && event.event_id.is_some() {
                        let color = if use_colors { "\x1b[33m" } else { "" };
                        line.push_str(
                            format!("{}{} {}- ", color, event.event_id().unwrap(), reset_ansi)
                                .as_str(),
                        );
                    }

                    if let Some(message) = event.message() {
                        line.push_str(message);
                    }

                    line.push_str("\n");
                    // NOTE: to ignore or not to ignore
                    writer.write(line.as_bytes()).await?;
                }
                crate::args::OutputType::Json => {
                    let mut json = json!({ "message": event.message() });

                    if write_timestamp {
                        if let Some(time) = event
                            .timestamp()
                            .and_then(|ts| parse_timestamp(ts, use_local_time))
                        {
                            json["timestamp"] = time.into();
                        }
                    }

                    if write_event_id {
                        json["id"] = event.event_id().into();
                    }

                    if write_group_name {
                        json["group"] = group_name.clone().into();
                    }

                    if write_stream_name {
                        json["stream"] = event.log_stream_name().into();
                    }

                    let mut line = json.to_string();
                    line.push_str("\n");

                    writer.write(line.as_bytes()).await?;
                }
            }
        }

        Ok(())
    }

    pub async fn query(&self, args: &QueryArgs, db: impl Database) -> eyre::Result<()> {
        let query = if let Some(path) = &args.file {
            if !path.exists() {
                return Err(eyre!("File provided via -file does not exist!"));
            }

            let mut file = File::open(path).await?;
            let mut query = String::new();
            file.read_to_string(&mut query).await?;
            query
        } else {
            let sample = "# vim: ft=lq\n";
            let query = open_in_editor(sample, None)?;

            query
                .strip_prefix(sample)
                .unwrap_or(query.as_str())
                .to_string()
        };

        let query_result = self
            .client
            .start_query()
            .set_log_group_names(Some(args.group_name.clone()))
            .query_string(&query)
            .start_time(
                args.start_time
                    // TODO: set start to 1h ago by default
                    .unwrap_or_else(|| (Utc::now().timestamp() - 30) * 1000),
            )
            .end_time(
                args.end_time
                    .unwrap_or_else(|| Utc::now().timestamp() * 1000),
            )
            .send()
            .await
            .wrap_err("Failed creating AWS CW Query Client.")?;

        // TODO: figure out why this is an option. shouldn't this always have an id at this
        // point?
        let query_id = query_result.query_id().unwrap();
        eprintln!("Starting query: {}", query_id);
        let mut history = QueryHistory::new(query_id.to_string(), query);
        db.save(&history).await?;

        loop {
            let output = self
                .client
                .get_query_results()
                .query_id(query_id)
                .send()
                .await?;

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

                    eprintln!("Status: {}", history.status);
                    eprintln!(
                        "Showing {} of {} records matched.",
                        history.records_total, history.records_matched,
                    );
                    let duration = history.modified_at - history.created_at;
                    eprintln!(
                        "{} records ({} bytes) scanned in {},{}s.",
                        history.records_scanned,
                        history.bytes_scanned,
                        duration.num_seconds(),
                        duration.num_milliseconds() - (duration.num_seconds() * 1000)
                    );

                    for line in results {
                        let mut json = Map::new();
                        for record in line {
                            if let Some(field) = record.field() {
                                // NOTE: Let's expose a flag wether to log the ptr or not.
                                if field == "@ptr" {
                                    continue;
                                }

                                // let parts: Vec<&str> = field.split(".").collect();
                                //
                                // if parts.len() == 1 {
                                //     json.insert(
                                //         field.to_string(),
                                //         Value::String(record.value().unwrap_or("").to_string()),
                                //     );
                                // } else {
                                //     for part in parts {}
                                // }

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
                    eprintln!("No status returned, unsure if I should proceed, exiting for now");
                    break;
                }
                _ => {
                    eprintln!("UNHANDLED: status: {}", output.status.unwrap());
                    break;
                }
            }
        }

        Ok(())
    }
}

fn parse_timestamp(timestamp_ms: i64, to_local_time: bool) -> Option<String> {
    if let Some(time) = DateTime::from_timestamp_millis(timestamp_ms) {
        if to_local_time {
            let local = time.with_timezone(&Local);
            return Some(local.to_rfc3339_opts(SecondsFormat::Secs, true));
        }

        return Some(time.to_rfc3339_opts(SecondsFormat::Secs, true));
    }

    None
}
