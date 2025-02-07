use std::time::UNIX_EPOCH;

use chrono::{DateTime, Local, SecondsFormat, Utc};

pub fn parse_human_time(h_time: &str) -> eyre::Result<i64> {
    if let Ok(duration) = humantime::parse_duration(h_time) {
        let now = Utc::now();
        let past_time = now - duration;

        Ok(past_time.timestamp() * 1000)
    } else {
        let time = humantime::parse_rfc3339_weak(h_time)?;
        let timestamp = time.duration_since(UNIX_EPOCH)?;
        let timestamp = i64::try_from(timestamp.as_millis())?;
        Ok(timestamp)
    }
}

pub fn parse_timestamp(timestamp_ms: i64, to_local_time: bool) -> Option<String> {
    if let Some(time) = DateTime::from_timestamp_millis(timestamp_ms) {
        if to_local_time {
            let local = time.with_timezone(&Local);
            return Some(local.to_rfc3339_opts(SecondsFormat::Secs, true));
        }

        return Some(time.to_rfc3339_opts(SecondsFormat::Secs, true));
    }

    None
}
