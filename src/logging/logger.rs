use eyre::Context;
use log::{LevelFilter, Log};

use super::{
    formatters::DefaultFormatter,
    sinks::{FileSink, NullSink, StderrSink},
    LogFormatter, LogSink,
};

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub datetime_format: String,
    pub use_ansi: bool,
}

impl Config {
    pub fn new() -> Self {
        Self {
            enabled: true,
            datetime_format: "%Y-%m-%d %H:%M:%S".to_string(),
            use_ansi: true,
        }
    }
}

pub struct Logger {
    filter: LevelFilter,
    sink: Box<dyn LogSink>,
    config: Config,
}

impl Logger {
    pub fn new(filter: LevelFilter, sink: Box<dyn LogSink>, config: Config) -> Self {
        Self {
            filter,
            sink,
            config,
        }
    }

    pub fn init(self) -> eyre::Result<()> {
        log::set_max_level(self.filter);
        log::set_boxed_logger(Box::new(self)).context("Failed registriging boxed logger")?;

        Ok(())
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.config.enabled && self.filter >= metadata.level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.sink.write_log(record).unwrap();
        }
    }

    fn flush(&self) {
        self.sink.flush()
    }
}

pub struct Builder {
    filter: LevelFilter,
    constructor:
        Box<dyn Fn(Box<dyn LogFormatter + 'static>) -> eyre::Result<Box<dyn LogSink + 'static>>>,
    formatter_builder: Box<dyn Fn(Config) -> Box<dyn LogFormatter + 'static>>,
    config: Config,
}

impl Builder {
    pub fn new() -> Self {
        Self {
            filter: LevelFilter::Off,
            constructor: Box::new(|_| Ok(Box::new(NullSink::new()))),
            formatter_builder: Box::new(|config| Box::new(DefaultFormatter::new(config))),
            config: Config::new(),
        }
    }

    pub fn with_level(self, filter: LevelFilter) -> Self {
        Self { filter, ..self }
    }

    pub fn with_file_sink(self, path: impl Into<String>) -> Self {
        let path: String = path.into();
        Self {
            constructor: Box::new(move |formatter| {
                let sink = FileSink::new(path.clone(), formatter)?;
                Ok(Box::new(sink))
            }),
            ..self
        }
    }

    pub fn with_stderr_sink(self) -> Self {
        Self {
            constructor: Box::new(move |formatter| {
                let sink = StderrSink::new(formatter);
                Ok(Box::new(sink))
            }),
            ..self
        }
    }

    pub fn build(&self) -> eyre::Result<Logger> {
        let sink = ((self.constructor)((self.formatter_builder)(self.config.clone())))?;
        Ok(Logger::new(self.filter, sink, self.config.clone()))
    }
}
