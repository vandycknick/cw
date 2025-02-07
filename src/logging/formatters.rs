use core::fmt;

use super::{logger::Config, LogFormatter};

pub struct DefaultFormatter {
    config: Config,
}

impl DefaultFormatter {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    fn timestamp(&self) -> String {
        let color = if self.config.use_ansi {
            "\x1b[0;90m"
        } else {
            ""
        };

        let time = chrono::Local::now().format(&self.config.datetime_format);
        format!("{}[{}]{}", color, time, self.reset())
    }

    fn format_level(&self, level: log::Level) -> &str {
        if self.config.use_ansi {
            match level {
                log::Level::Error => "\x1b[0;31mERR\x1b[0m",
                log::Level::Warn => "\x1b[0;33mWRN\x1b[0m",
                log::Level::Info => "\x1b[0;32mINF\x1b[0m",
                log::Level::Debug => "\x1b[0;34mDEB\x1b[0m",
                log::Level::Trace => "\x1b[0;37mTRC\x1b[0m",
            }
        } else {
            match level {
                log::Level::Error => "ERR",
                log::Level::Warn => "WRN",
                log::Level::Info => "INF",
                log::Level::Debug => "DEB",
                log::Level::Trace => "TRC",
            }
        }
    }

    fn reset(&self) -> &str {
        if self.config.use_ansi {
            "\x1b[0m"
        } else {
            ""
        }
    }

    fn format_msg<'a>(&self, args: &fmt::Arguments<'a>) -> String {
        let color = if self.config.use_ansi {
            "\x1b[0;1m"
        } else {
            ""
        };

        format!("{}{}{}", color, args, self.reset())
    }
}

impl LogFormatter for DefaultFormatter {
    fn format(&self, record: &log::Record) -> String {
        format!(
            "{} {}: {}",
            self.timestamp(),
            self.format_level(record.level()),
            self.format_msg(record.args()),
        )
    }
}
