use std::path::PathBuf;

use chrono::Utc;
use clap::{Parser, ValueEnum};

#[derive(Parser)]
#[command(version)]
#[command(about = "Swiss army knife to query CloudWatch logs form the CLI.", long_about = None, disable_help_subcommand = true)]
pub struct GlobalArgs {
    #[arg(global = true, long)]
    pub profile: Option<String>,

    #[arg(global = true, long)]
    pub verbose: bool,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Parser)]
pub enum Command {
    #[command(subcommand)]
    Ls(LsCommand),

    Tail(TailArgs),

    Query(QueryArgs),
}

#[derive(Parser)]
pub enum LsCommand {
    Groups,
    Streams { group_name: String },
}

#[derive(Parser, Clone, Debug)]
pub struct TailArgs {
    #[arg(index = 1, value_name = "groupName[:logStreamPrefix]")]
    pub group_and_stream_prefix: String,

    #[arg(short, long)]
    pub follow: bool,

    #[arg(short = 'g', long)]
    pub filter: Option<String>,

    #[arg(short, long, value_parser = parse_human_time)]
    pub start_time: Option<i64>,

    #[arg(short, long)]
    pub timestamp: bool,

    #[arg(short = 'i', long)]
    pub event_id: bool,

    #[arg(long)]
    pub stream_name: bool,

    #[arg(long)]
    pub group_name: bool,

    #[arg(long, short, value_enum, default_value_t=OutputType::Text)]
    pub output: OutputType,

    #[arg(short, long)]
    pub local: bool,
}

#[derive(Parser, Clone, Debug)]
pub struct QueryArgs {
    #[arg(short, long, required = true)]
    pub group_name: Vec<String>,

    #[arg(long, group = "from_id")]
    pub query_id: Option<String>,

    #[arg(short, long, group = "from_file")]
    pub file: Option<PathBuf>,

    #[arg(short, long, value_parser = parse_human_time)]
    pub start_time: Option<i64>,

    #[arg(short, long, value_parser = parse_human_time)]
    pub end_time: Option<i64>,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum OutputType {
    Text,
    Json,
}

impl TailArgs {
    pub fn parse_group_and_stream(&self) -> (Option<String>, Option<String>) {
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
}

// TODO: Add ability to parse iso formatted dates
fn parse_human_time(h_time: &str) -> eyre::Result<i64> {
    let duration = humantime::parse_duration(h_time)?;

    let now = Utc::now();
    let past_time = now - duration;

    Ok(past_time.timestamp() * 1000)
}
