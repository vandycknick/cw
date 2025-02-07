mod formatters;
mod logger;
mod sinks;

pub use logger::Builder;

pub trait LogFormatter: Sync + Send {
    fn format(&self, record: &log::Record) -> String;
}

pub trait LogSink: Sync + Send {
    fn write_log(&self, record: &log::Record) -> eyre::Result<()>;
    fn flush(&self);
}
