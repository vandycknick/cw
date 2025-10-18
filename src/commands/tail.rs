use std::fmt::Write;
use std::{future::Future, time::Duration};

use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
use aws_sdk_cloudwatchlogs::Client;
use chrono::Utc;
use clap::{Parser, ValueEnum};
use eyre::Context;
use futures_util::{stream::FuturesUnordered, StreamExt};
use serde_json::json;
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    task::JoinHandle,
};
use yansi::Paint;

use crate::utils::{parse_human_time, parse_timestamp};

use super::LogClientBuilder;

#[derive(Debug, Clone)]
pub struct LogGroupRef(String, Option<String>);

impl LogGroupRef {
    pub fn new(group_name: &str, stream_name: &str) -> eyre::Result<Self> {
        let group_name = group_name.trim();
        let stream_name = stream_name.trim();

        if group_name.is_empty() {
            return Err(eyre::eyre!("Group name cannot be empty"));
        }

        Ok(Self(
            group_name.to_string(),
            if stream_name.is_empty() {
                None
            } else {
                Some(stream_name.to_string())
            },
        ))
    }

    pub fn parse(groups_with_stream_prefix: &str) -> eyre::Result<Vec<Self>> {
        groups_with_stream_prefix
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                let (group, stream) = s.split_once(':').unwrap_or((s, ""));
                Self::new(group, stream).map_err(|e| eyre::eyre!("Invalid group '{}': {}", s, e))
            })
            .collect()
    }
}

#[derive(Clone, PartialEq, Debug)]
struct LogEvent {
    pub group_name: String,
    pub log_stream_name: Option<String>,
    pub timestamp: Option<i64>,
    pub message: Option<String>,
    pub ingestion_time: Option<i64>,
    pub event_id: Option<String>,
}

impl From<(&str, &FilteredLogEvent)> for LogEvent {
    fn from((group_name, event): (&str, &FilteredLogEvent)) -> Self {
        Self {
            group_name: group_name.to_owned(),
            log_stream_name: event.log_stream_name.clone(),
            timestamp: event.timestamp,
            message: event.message.clone(),
            ingestion_time: event.ingestion_time,
            event_id: event.event_id.clone(),
        }
    }
}

#[derive(ValueEnum, Clone, Debug)]
pub enum OutputType {
    Text,
    Json,
}

trait LogEventWriter {
    fn write<'a>(
        &'a mut self,
        event: &'a LogEvent,
    ) -> impl Future<Output = eyre::Result<()>> + Send + 'a;
}

struct TextWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    use_local_time: bool,
    with_timestamp: bool,
    with_group_name: bool,
    with_stream_name: bool,
    with_event_id: bool,

    sink: W,
}

impl<W> TextWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(
        use_local_time: bool,
        with_timestamp: bool,
        with_group_name: bool,
        with_stream_name: bool,
        with_event_id: bool,
        sink: W,
    ) -> Self {
        Self {
            use_local_time,
            with_timestamp,
            with_group_name,
            with_stream_name,
            with_event_id,
            sink,
        }
    }
}

impl<W> LogEventWriter for TextWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    async fn write(&mut self, event: &LogEvent) -> eyre::Result<()> {
        let mut line = String::new();

        if self.with_timestamp {
            if let Some(time) = event
                .timestamp
                .and_then(|ts| parse_timestamp(ts, self.use_local_time))
            {
                write!(&mut line, "{} - ", time.green())?;
            }
        }

        if self.with_group_name {
            write!(&mut line, "{} - ", event.group_name.blue())?;
        }

        if self.with_stream_name {
            if let Some(stream_name) = event.log_stream_name.as_deref() {
                write!(&mut line, "{} - ", stream_name.cyan())?;
            }
        }

        if self.with_event_id {
            if let Some(event_id) = event.event_id.as_deref() {
                write!(&mut line, "{} - ", event_id.yellow())?;
            }
        }

        if let Some(msg) = &event.message {
            line.push_str(msg);
        }

        line.push('\n');
        self.sink
            .write_all(line.as_bytes())
            .await
            .context("failed to write to sink")
    }
}

struct JsonWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    use_local_time: bool,
    with_timestamp: bool,
    with_group_name: bool,
    with_stream_name: bool,
    with_event_id: bool,

    sink: W,
}

impl<W> JsonWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(
        use_local_time: bool,
        with_timestamp: bool,
        with_group_name: bool,
        with_stream_name: bool,
        with_event_id: bool,
        sink: W,
    ) -> Self {
        Self {
            use_local_time,
            with_timestamp,
            with_group_name,
            with_stream_name,
            with_event_id,
            sink,
        }
    }
}

impl<W> LogEventWriter for JsonWriter<W>
where
    W: AsyncWrite + Unpin + Send,
{
    async fn write(&mut self, event: &LogEvent) -> eyre::Result<()> {
        let mut json = json!({ "message": event.message });

        if self.with_timestamp {
            if let Some(time) = event
                .timestamp
                .and_then(|ts| parse_timestamp(ts, self.use_local_time))
            {
                json["timestamp"] = time.into();
            }
        }

        if self.with_event_id {
            if let Some(id) = &event.event_id {
                json["id"] = id.clone().into();
            }
        }

        if self.with_group_name {
            json["group"] = event.group_name.clone().into();
        }

        if self.with_stream_name {
            if let Some(stream) = &event.log_stream_name {
                json["stream"] = stream.clone().into();
            }
        }

        let mut line = json.to_string();
        line.push('\n');
        self.sink
            .write_all(line.as_bytes())
            .await
            .context("failed to write to sink")
    }
}

#[derive(Parser, Clone, Debug)]
pub struct Cmd {
    #[arg(index = 1, value_name = "groupName[:logStreamPrefix][,...]")]
    pub groups_and_stream_prefix: String,

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

    #[arg(short, long = "timestamp", help = "Print the event timestamp.")]
    pub print_timestamp: bool,

    #[arg(short = 'i', long = "event-id", help = "Print the event id.")]
    pub print_event_id: bool,

    #[arg(
        long = "stream-name",
        help = "Print the log stream name that this event belongs to."
    )]
    pub print_stream_name: bool,

    #[arg(
        long = "group-name",
        help = "Print the log group name that this event belongs to."
    )]
    pub print_group_name: bool,

    #[arg(long, short, value_enum, default_value_t=OutputType::Text)]
    pub output: OutputType,

    #[arg(short, long, help = "Treat date and time in local timezone.")]
    pub local: bool,
}

impl Cmd {
    pub async fn run(&self, builder: &LogClientBuilder) -> eyre::Result<()> {
        let log_group_refs = LogGroupRef::parse(&self.groups_and_stream_prefix)?;
        let client = builder.build().await?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut tasks = FuturesUnordered::<JoinHandle<eyre::Result<()>>>::new();

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

        for LogGroupRef(group_name, stream_name) in &log_group_refs {
            let log_producer = tokio::spawn(Self::tail_log_producer(
                client.clone(),
                sender.clone(),
                start_time,
                self.end_time,
                self.filter.clone(),
                self.follow,
                group_name.into(),
                stream_name.clone(),
            ));
            tasks.push(log_producer);
        }
        drop(sender); // NOTE: dropping here because each producers already has a clone

        let sink = tokio::io::stdout();
        let log_writer = match self.output {
            OutputType::Text => {
                let w = TextWriter::new(
                    self.local,
                    self.print_timestamp,
                    self.print_group_name,
                    self.print_stream_name,
                    self.print_event_id,
                    sink,
                );
                tokio::spawn(Self::write_log_event(receiver, w))
            }
            OutputType::Json => {
                let w = JsonWriter::new(
                    self.local,
                    self.print_timestamp,
                    self.print_group_name,
                    self.print_stream_name,
                    self.print_event_id,
                    sink,
                );
                tokio::spawn(Self::write_log_event(receiver, w))
            }
        };
        tasks.push(log_writer);

        while let Some(res) = tasks.next().await {
            match res {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => {
                    for handle in tasks.into_iter() {
                        handle.abort();
                    }
                    return Err(e);
                }
                Err(e) => {
                    for handle in tasks.into_iter() {
                        handle.abort();
                    }
                    return Err(eyre::eyre!(e));
                }
            }
        }

        Ok(())
    }

    async fn tail_log_producer(
        client: Client,
        sender: UnboundedSender<LogEvent>,
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
                .context("Failed to fetch CloudWatch logs.")?;

            let events = response.events();
            for event in events {
                // NOTE: This only errors if the receiver is dropped or closed. If this happens
                // there's no point in continuing to process anymore events.
                sender.send((group_name.as_str(), event).into())?;
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
        mut receiver: UnboundedReceiver<LogEvent>,
        mut writer: impl LogEventWriter,
    ) -> eyre::Result<()> {
        tracing::info!(target: "cw", "starting tail log writer");

        while let Some(event) = receiver.recv().await {
            writer.write(&event).await?;
        }

        Ok(())
    }
}
