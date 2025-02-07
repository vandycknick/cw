use std::fmt::Display;

use aws_sdk_cloudwatchlogs as cloudwatchlogs;
use chrono::{DateTime, Utc};
use chronoutil::RelativeDuration;
use clap::{command, Subcommand};
use eyre::Context;

use super::LogClientBuilder;

#[derive(Subcommand, Debug)]
#[command(infer_subcommands = true)]
pub enum Cmd {
    Groups,
    Streams { group_name: String },
}

impl Display for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Groups => write!(f, "groups"),
            Cmd::Streams { group_name } => write!(f, "streams <{}>", group_name),
        }
    }
}

impl Cmd {
    pub async fn run(&self, builder: &LogClientBuilder) -> eyre::Result<()> {
        let client = builder.build().await?;
        match self {
            Self::Groups => self.list_groups(&client).await,
            Self::Streams { group_name } => self.list_streams(&client, group_name).await,
        }
    }

    pub async fn list_groups(&self, client: &cloudwatchlogs::Client) -> eyre::Result<()> {
        let mut next_token: Option<String> = None;

        loop {
            let mut request_builder = client
                .describe_log_groups()
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

            let six_months = RelativeDuration::months(6);
            let sm_ago = Utc::now() - six_months;

            let response = request_builder
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
}
