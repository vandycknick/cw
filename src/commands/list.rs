use std::fmt::Display;

use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use chrono::{DateTime, Days, Months, Utc};
use clap::{command, Subcommand};
use eyre::Context;

use super::LogClientBuilder;

#[derive(Subcommand, Debug)]
#[command(infer_subcommands = false)]
pub enum Cmd {
    Groups {
        filter: Option<String>,
    },
    Streams {
        group_name: String,

        #[arg(
            short,
            long,
            help = "Log streams that have exceeded the log group's retention period are considered expired and are filtered. Add this flag to show all streams."
        )]
        show_expired: bool,
    },
}

impl Display for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Groups { filter: _ } => write!(f, "groups"),
            Cmd::Streams {
                group_name,
                show_expired: _,
            } => write!(f, "streams <{}>", group_name),
        }
    }
}

impl Cmd {
    pub async fn run(&self, builder: &LogClientBuilder) -> eyre::Result<()> {
        let client = builder.build().await?;
        match self {
            Self::Groups { filter } => self.list_groups(&client, filter).await,
            Self::Streams {
                group_name,
                show_expired: _,
            } => self.list_streams(&client, group_name).await,
        }
    }

    pub async fn list_groups(
        &self,
        client: &cloudwatchlogs::Client,
        filter: &Option<String>,
    ) -> eyre::Result<()> {
        let mut next_token: Option<String> = None;

        loop {
            let mut request_builder = client
                .describe_log_groups()
                .set_log_group_name_pattern(filter.clone())
                // NOTE: 50 is the maximum, ref: https://docs.aws.amazon.com/AmazonCloudWatchLogs/latest/APIReference/API_DescribeLogGroups.html#CWL-DescribeLogGroups-request-limit
                .limit(50);

            if let Some(ref token) = next_token {
                request_builder = request_builder.next_token(token);
            }

            let response = request_builder
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

    pub async fn list_streams(
        &self,
        client: &cloudwatchlogs::Client,
        group_name: impl Into<String>,
    ) -> eyre::Result<()> {
        let mut next_token: Option<String> = None;
        let group_name = group_name.into();

        let log_groups = client
            .describe_log_groups()
            .log_group_name_prefix(&group_name)
            .send()
            .await?;

        let log_group = if let Some(g) = log_groups
            .log_groups()
            .iter()
            .filter(|l| l.log_group_name() == Some(&group_name))
            .next()
        {
            g
        } else {
            return Err(eyre::eyre!("Can't find log group with name {}", group_name));
        };

        let retention = if let Some(days) = log_group.retention_in_days() {
            log::info!(target: "cw", "The retention for {} is set to {}.", group_name, days);
            Utc::now().checked_sub_days(Days::new(days as u64))
        } else {
            log::info!(target: "cw", "No retention found for {}, only showing streams that received an event in the last 6 months.", group_name);
            Utc::now().checked_sub_months(Months::new(6))
        };

        loop {
            let mut request_builder = client
                .describe_log_streams()
                .log_group_identifier(&group_name)
                .order_by(cloudwatchlogs::types::OrderBy::LastEventTime)
                .descending(true)
                // NOTE: 50 is the maximum, ref:
                .limit(50);

            if let Some(ref token) = next_token {
                request_builder = request_builder.next_token(token);
            }

            let response = request_builder
                .send()
                .await
                .wrap_err("Failed creating AWS Client.")?;

            let streams = response.log_streams().iter().filter(|s| {
                s.last_event_timestamp()
                    .map_or(false, |t| DateTime::from_timestamp_millis(t) > retention)
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
}
