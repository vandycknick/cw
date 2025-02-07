use std::path::PathBuf;

pub trait ConfigManager: Sized + Clone + Send + Sync {
    fn get_db_path(&self) -> eyre::Result<String>;
    fn get_log_path(&self) -> eyre::Result<String>;
}

#[derive(Default, Clone, Debug)]
pub struct LocalConfigManager {}

impl LocalConfigManager {
    pub fn new() -> Self {
        Self {}
    }
}

// NOTE: This requires HOME to be set. Given how I expect the tool to be used this is a
// reasonable expectation. I could fallback to the C getpwuid api, but then I need libc or nix
// package. I rather not pay the cost for this. Also it means I would need to do the same for
// Windows.
#[cfg(not(target_os = "windows"))]
pub fn home_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("$HOME not found");
    PathBuf::from(home)
}

#[cfg(target_os = "windows")]
pub fn home_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE").expect("%userprofile% not found");
    PathBuf::from(home)
}

pub fn data_dir() -> PathBuf {
    let data_dir = std::env::var("XDG_DATA_HOME")
        .map_or_else(|_| home_dir().join(".local").join("share"), PathBuf::from);

    data_dir.join("cw")
}

pub fn cache_dir() -> PathBuf {
    let data_dir = std::env::var("XDG_CACHE_HOME")
        .map_or_else(|_| home_dir().join(".local").join("cache"), PathBuf::from);

    data_dir.join("cw")
}

impl ConfigManager for LocalConfigManager {
    fn get_db_path(&self) -> eyre::Result<String> {
        let mut cw_data_dir = data_dir();

        std::fs::create_dir_all(&cw_data_dir)?;

        cw_data_dir.push("db.sqlite3");

        match cw_data_dir.to_str() {
            Some(data) => Ok(data.to_string()),
            None => Err(eyre::eyre!("Can't construct db path in data dir!")),
        }
    }

    fn get_log_path(&self) -> eyre::Result<String> {
        let mut cw_cache_dir = cache_dir();

        std::fs::create_dir_all(&cw_cache_dir)?;

        cw_cache_dir.push("cw.log");

        match cw_cache_dir.to_str() {
            Some(data) => Ok(data.to_string()),
            None => Err(eyre::eyre!("Can't construct cw.log path in cache dir!")),
        }
    }
}
