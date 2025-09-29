use std::{io::IsTerminal, time::Duration};

use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
use aws_sdk_cloudwatchlogs::Client;
use chrono::Utc;
use clap::{Parser, ValueEnum};
use eyre::{Context, OptionExt};
use serde_json::json;
use tokio::{
    io::AsyncWriteExt,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use crate::utils::{parse_human_time, parse_timestamp};

use super::LogClientBuilder;

#[derive(ValueEnum, Clone, Debug)]
pub enum OutputType {
    Text,
    Json,
}

#[derive(Parser, Clone, Debug)]
pub struct Cmd {
    #[arg(index = 1, value_name = "groupName[:logStreamPrefix]")]
    pub group_and_stream_prefix: String,

    #[arg(
        short,
        long,
        value_parser = parse_human_time,
        help="The UTC start time. Passed as either date/time or human-friendly format.",
    )]
    pub start_time: Option<i64>,

    #[arg(
        short,
        long,
        value_parser = parse_human_time,
        help="The UTC end time. Passed as either date/time or human-friendly format.",
    )]
    pub end_time: Option<i64>,

    #[arg(short, long, help = "Tail or continue following the logs.")]
    pub follow: bool,

    #[arg(
        short = 'g',
        long,
        alias = "grep",
        help = "Pattern to filter logs by. See http://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/FilterAndPatternSyntax.html for syntax."
    )]
    pub filter: Option<String>,

    #[arg(short, long, help = "Print the event timestamp.")]
    pub timestamp: bool,

    #[arg(short = 'i', long, help = "Print the event id.")]
    pub event_id: bool,

    #[arg(long, help = "Print the log stream name that this event belongs to.")]
    pub stream_name: bool,

    #[arg(long, help = "Print the log group name that this event belongs to.")]
    pub group_name: bool,

    #[arg(long, short, value_enum, default_value_t=OutputType::Text)]
    pub output: OutputType,

    #[arg(short, long, help = "Treat date and time in local timezone.")]
    pub local: bool,
}

impl Cmd {
    pub async fn run(&self, builder: &LogClientBuilder) -> eyre::Result<()> {
        let client = builder.build().await?;
        let (group_name, stream_name) = self.parse_group_and_stream();

        let group_name = group_name.ok_or_eyre(format!(
            "Invalid group or stream name, group name can't be empty: {}",
            self.group_and_stream_prefix
        ))?;

        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        let start_time = self
            .start_time
            // NOTE: Moving `now` slightly into the past. That way it's more
            // likely that an empty start time atleast returns something.
            .unwrap_or_else(|| (Utc::now().timestamp() - 30) * 1000);

        if self.end_time.is_some() && self.follow {
            return Err(eyre::eyre!(
                "You can not use --end-time together with --follow!"
            ));
        }

        let log_producer = tokio::spawn(Self::tail_log_producer(
            client.clone(),
            sender,
            start_time,
            self.end_time,
            self.filter.clone(),
            self.follow,
            group_name.clone(),
            stream_name,
        ));

        let log_writer = tokio::spawn(Self::write_log_event(
            receiver,
            self.output.clone(),
            group_name.clone(),
            self.local,
            self.timestamp,
            self.group_name,
            self.stream_name,
            self.event_id,
        ));

        // TODO: This way if both producer and writer trip up. Only the producer error ever gets
        // bubbled up. Might be nicer to collect them both. And return a nicely unfied formatted
        // error message.
        log_producer.await??;
        log_writer.await??;

        Ok(())
    }

    fn parse_group_and_stream(&self) -> (Option<String>, Option<String>) {
        let (group_name, stream_name) = match self.group_and_stream_prefix.split_once(":") {
            Some(matches) => matches,
            None => (self.group_and_stream_prefix.as_str(), ""),
        };

        if group_name == "" {
            return (None, None);
        }

        if stream_name == "" {
            return (Some(group_name.into()), None);
        }

        return (Some(group_name.into()), Some(stream_name.into()));
    }

    async fn tail_log_producer(
        client: Client,
        sender: UnboundedSender<FilteredLogEvent>,
        start_time: i64,
        end_time: Option<i64>,
        filter: Option<String>,
        follow: bool,
        group_name: String,
        stream_name: Option<String>,
    ) -> eyre::Result<()> {
        tracing::info!(target: "cw", "starting tail log producer");
        let mut tail_sleep_sec = 1;
        let mut start_time = start_time;
        let mut next_token: Option<String> = None;
        let mut builder = client
            .filter_log_events()
            .log_group_name(&group_name)
            .limit(10_000); // INFO: This is the default value.

        if let Some(stream_name) = &stream_name {
            builder = builder.log_stream_name_prefix(stream_name);
        }

        if let Some(filter_pattern) = &filter {
            builder = builder.filter_pattern(filter_pattern);
        }

        loop {
            tracing::trace!(
                target: "cw",
                "Getting logs from start ({}) until end ({:?}) with token {:?}.",
                start_time,
                end_time,
                next_token
            );
            let response = builder
                .clone()
                .start_time(start_time)
                .set_end_time(end_time)
                .set_next_token(next_token)
                .send()
                .await
                .context("Can't create AWS client")?;

            let events = response.events();
            for event in events {
                // NOTE: This only errors if the receiver is dropped or closed. If this happens
                // there's no point in continuing to process anymore events.
                sender.send(event.clone())?;
            }

            next_token = response.next_token().map(|s| s.to_string());
            if next_token == None && !follow {
                break;
            }

            // NOTE: move pointer past the last returned event to prevent us from returning
            // duplicated log lines.
            if let Some(timestamp) = &events.last().and_then(|e| e.timestamp()) {
                start_time = timestamp + 1;
            }

            if events.len() == 0 && follow {
                tracing::debug!(
                    target: "cw",
                    "Reached at of stream while tailing, sleeping for {} sec",
                    tail_sleep_sec
                );
                tokio::time::sleep(Duration::from_secs(tail_sleep_sec)).await;
                tail_sleep_sec = (tail_sleep_sec + 1).clamp(1, 10);
            } else {
                tail_sleep_sec = 1;
            }
        }
        Ok(())
    }

    async fn write_log_event(
        receiver: UnboundedReceiver<FilteredLogEvent>,
        output_type: OutputType,
        group_name: String,
        use_local_time: bool,
        write_timestamp: bool,
        write_group_name: bool,
        write_stream_name: bool,
        write_event_id: bool,
    ) -> eyre::Result<()> {
        tracing::info!(target: "cw", "starting tail log writer");

        let mut writer = tokio::io::stdout();
        let use_colors = std::io::stdout().is_terminal();
        let mut receiver = receiver;
        let reset_ansi = if use_colors { "\x1b[0m" } else { "" };

        while let Some(event) = receiver.recv().await {
            match output_type {
                OutputType::Text => {
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
                OutputType::Json => {
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
}
