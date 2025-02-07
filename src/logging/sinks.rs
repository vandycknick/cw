use std::{
    fs::File,
    io::{LineWriter, Write},
    path::PathBuf,
    sync::Mutex,
};

use eyre::Context;

use super::{LogFormatter, LogSink};

pub struct FileSink {
    file: Mutex<LineWriter<File>>,
    file_path: PathBuf,
    formatter: Box<dyn LogFormatter>,
    max_file_size: Option<u64>,
}

impl FileSink {
    pub fn new(path: impl Into<String>, formatter: Box<dyn LogFormatter>) -> eyre::Result<Self> {
        let path: &str = &path.into();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed opening or creating log file {}", path))?;

        Ok(Self {
            file: Mutex::new(LineWriter::new(file)),
            file_path: PathBuf::from(path),
            formatter,
            // NOTE: not used at the moment,
            max_file_size: None,
        })
    }

    fn rotate_if_exceeds_max_file_size(&self) {
        if self.max_file_size.is_none() {
            return;
        }

        let mut file = self.file.lock().unwrap();

        let md = file.get_ref().metadata().unwrap();

        if md.len() > self.max_file_size.unwrap() {
            let path = self.file_path.to_str().unwrap();

            let mut new_path = format!("{}.old", path);

            let mut counter = 1;
            while std::fs::metadata(&new_path).is_ok() {
                new_path = format!("{}.old{}", path, counter);
                counter += 1;
            }

            std::fs::rename(path, &new_path).unwrap();

            let new_file = std::fs::File::create(path).unwrap();
            *file = LineWriter::new(new_file);
        }
    }
}

impl LogSink for FileSink {
    fn write_log(&self, record: &log::Record) -> eyre::Result<()> {
        if record.target() != "cw" {
            return Ok(());
        }

        self.rotate_if_exceeds_max_file_size();

        let mut file = self.file.lock().map_err(|e| eyre::eyre!(e.to_string()))?;
        writeln!(file, "{}", self.formatter.format(record))?;
        file.flush().context("Can't flush file")
    }

    fn flush(&self) {
        self.file.lock().unwrap().flush().unwrap()
    }
}

pub struct StderrSink {
    handle: std::io::Stderr,
    formatter: Box<dyn LogFormatter>,
}

impl StderrSink {
    pub fn new(formatter: Box<dyn LogFormatter>) -> Self {
        Self {
            handle: std::io::stderr(),
            formatter,
        }
    }
}

impl LogSink for StderrSink {
    fn write_log(&self, record: &log::Record) -> eyre::Result<()> {
        let mut writer = self.handle.lock();

        writeln!(writer, "{}", self.formatter.format(record))?;
        writer.flush().context("Can't flush file")
    }

    fn flush(&self) {
        self.handle.lock().flush().unwrap()
    }
}

pub struct NullSink {}

impl NullSink {
    pub fn new() -> Self {
        Self {}
    }
}

impl LogSink for NullSink {
    fn write_log(&self, _record: &log::Record) -> eyre::Result<()> {
        Ok(())
    }

    fn flush(&self) {}
}
