use std::{
    fmt::{Debug, Display},
    fs,
    path::Path,
    str::FromStr,
    time::Duration,
};

use chrono::{DateTime, Utc};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};
use uuid::Uuid;

#[derive(Default, Clone, Debug)]
pub struct QueryHistory {
    id: String,
    pub query_id: String,
    pub contents: String,
    pub status: QueryStatus,

    pub records_total: i64,
    pub records_matched: f64,
    pub records_scanned: f64,
    pub bytes_scanned: f64,

    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl QueryHistory {
    pub fn new(query_id: String, contents: String) -> Self {
        let uuid = Uuid::new_v4().as_simple().to_string();
        let now = Utc::now();

        Self {
            id: uuid.to_string(),
            query_id,
            contents,
            created_at: now,
            modified_at: now,
            ..Default::default()
        }
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    pub fn set_status(&mut self, status: QueryStatus) {
        self.status = status;
        self.modified_at = Utc::now();
    }

    pub fn set_statistics(
        &mut self,
        records_total: i64,
        records_matched: f64,
        records_scanned: f64,
        bytes_scanned: f64,
    ) {
        self.records_total = records_total;
        self.records_matched = records_matched;
        self.records_scanned = records_scanned;
        self.bytes_scanned = bytes_scanned;
        self.modified_at = Utc::now();
    }
}

#[derive(Clone, Debug)]
pub enum QueryStatus {
    Scheduled,
    Running,
    Complete,
    Failed,
    Timeout,
}

impl Default for QueryStatus {
    fn default() -> Self {
        Self::Scheduled
    }
}

impl Display for QueryStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryStatus::Scheduled => write!(f, "Scheduled"),
            QueryStatus::Running => write!(f, "Running"),
            QueryStatus::Complete => write!(f, "Complete"),
            QueryStatus::Failed => write!(f, "Failed"),
            QueryStatus::Timeout => write!(f, "Timeout"),
        }
    }
}

pub trait Database: Sized + Clone + Send + Sync + 'static {
    type Settings: Debug + Clone + Send + Sync + 'static;
    async fn new(settings: &Self::Settings) -> eyre::Result<Self>;
    async fn save(&self, history: &QueryHistory) -> eyre::Result<()>;
    async fn update(&self, history: &QueryHistory) -> eyre::Result<()>;
}

#[derive(Debug, Clone)]
pub struct Sqlite {
    pool: SqlitePool,
}

impl Sqlite {
    pub async fn sqlite_version(&self) -> eyre::Result<String> {
        let result: String = sqlx::query_scalar("SELECT sqlite_version()")
            .fetch_one(&self.pool)
            .await?;

        Ok(result)
    }

    async fn setup_db(pool: &SqlitePool) -> eyre::Result<()> {
        sqlx::migrate!("./migrations").run(pool).await?;

        Ok(())
    }
}

impl Database for Sqlite {
    type Settings = String;

    async fn new(path: &Self::Settings) -> eyre::Result<Self> {
        let path = Path::new(path);
        // dbg!("opening sqlite database at {:?}", path);

        let create = !path.exists();
        if create {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
        }

        let opts = SqliteConnectOptions::from_str(path.as_os_str().to_str().unwrap())?
            .journal_mode(SqliteJournalMode::Wal)
            .optimize_on_close(true, None)
            .synchronous(SqliteSynchronous::Normal)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(opts)
            .await?;

        Self::setup_db(&pool).await?;

        Ok(Self { pool })
    }

    async fn save(&self, history: &QueryHistory) -> eyre::Result<()> {
        // dbg!("saving history to sqlite");
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "insert or ignore into query_history(
                id, query_id, contents, status,
                records_total, records_matched, records_scanned, bytes_scanned,
                created_at, modified_at, deleted_at
            )
            values(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(history.id.as_str())
        .bind(history.query_id.as_str())
        .bind(history.contents.as_str())
        .bind(history.status.to_string().as_str())
        .bind(history.records_total)
        .bind(history.records_matched)
        .bind(history.records_scanned)
        .bind(history.bytes_scanned)
        .bind(history.created_at)
        .bind(history.modified_at)
        .bind(history.deleted_at)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }

    async fn update(&self, history: &QueryHistory) -> eyre::Result<()> {
        // dbg!("updating query_history in sqlite");
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "update query_history set
                    query_id        = ?2,
                    contents        = ?3,
                    status          = ?4,
                    records_total   = ?5,
                    records_matched = ?6,
                    records_scanned = ?7,
                    bytes_scanned   = ?8,
                    created_at      = ?9,
                    modified_at     = ?10,
                    deleted_at      = ?11
                where id = ?1",
        )
        .bind(history.id.as_str())
        .bind(history.query_id.as_str())
        .bind(history.contents.as_str())
        .bind(history.status.to_string().as_str())
        .bind(history.records_total)
        .bind(history.records_matched)
        .bind(history.records_scanned)
        .bind(history.bytes_scanned)
        .bind(history.created_at)
        .bind(history.modified_at)
        .bind(history.deleted_at)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }
}
