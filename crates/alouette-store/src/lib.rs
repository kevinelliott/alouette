//! Alouette storage.
//!
//! * [`Store`]: one SQLite file for history, bookmarks and small key-value state.
//!   SQLite is compiled in, so there is no system dependency.
//! * [`secrets`]: passwords and tokens live in the OS keychain, never in SQLite or the config.
//!
//! The optional offline cache for mail and news arrives with those modules and stays off
//! by default.

pub mod secrets;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use time::OffsetDateTime;

pub use secrets::{SecretError, SecretStore};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("could not create {path}: {source}")]
    Io { path: String, source: std::io::Error },
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// One visited URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
    pub module: String,
    pub visited_at: OffsetDateTime,
}

/// A saved URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub tags: Vec<String>,
    pub created_at: OffsetDateTime,
}

/// Schema changes, applied in order. The index + 1 is the schema version.
const MIGRATIONS: &[&str] = &[
    // 1: history, bookmarks and key-value state.
    "CREATE TABLE history (
        id INTEGER PRIMARY KEY,
        url TEXT NOT NULL,
        title TEXT NOT NULL DEFAULT '',
        module TEXT NOT NULL DEFAULT '',
        visited_at INTEGER NOT NULL
     );
     CREATE INDEX history_visited ON history(visited_at DESC);
     CREATE INDEX history_url ON history(url);
     CREATE TABLE bookmarks (
        url TEXT PRIMARY KEY,
        title TEXT NOT NULL DEFAULT '',
        tags TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL
     );
     CREATE TABLE kv (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );",
];

/// The SQLite state database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the database at `path` and bring its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|source| StoreError::Io { path: dir.display().to_string(), source })?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    /// A private in-memory database, for tests and guest sessions.
    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Current schema version.
    pub fn schema_version(&self) -> Result<usize> {
        Ok(self.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))? as usize)
    }

    fn migrate(&mut self) -> Result<()> {
        let current = self.schema_version()?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
            tracing::info!(version = i + 1, "applied state database migration");
        }
        Ok(())
    }

    // ---- history ----

    pub fn add_history(&self, url: &str, title: &str, module: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO history (url, title, module, visited_at) VALUES (?1, ?2, ?3, ?4)",
            params![url, title, module, now()],
        )?;
        Ok(())
    }

    /// Most recent visits first, one row per URL.
    pub fn recent_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, title, module, MAX(visited_at) AS v FROM history
             GROUP BY url ORDER BY v DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(HistoryEntry { url: r.get(0)?, title: r.get(1)?, module: r.get(2)?, visited_at: from_unix(r.get(3)?) })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn clear_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM history", [])?;
        Ok(())
    }

    // ---- bookmarks ----

    /// Add or update a bookmark.
    pub fn set_bookmark(&self, url: &str, title: &str, tags: &[&str]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (url, title, tags, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(url) DO UPDATE SET title = excluded.title, tags = excluded.tags",
            params![url, title, tags.join(","), now()],
        )?;
        Ok(())
    }

    pub fn remove_bookmark(&self, url: &str) -> Result<bool> {
        Ok(self.conn.execute("DELETE FROM bookmarks WHERE url = ?1", params![url])? > 0)
    }

    pub fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        let mut stmt = self.conn.prepare("SELECT url, title, tags, created_at FROM bookmarks ORDER BY title, url")?;
        let rows = stmt.query_map([], |r| {
            let tags: String = r.get(2)?;
            Ok(Bookmark {
                url: r.get(0)?,
                title: r.get(1)?,
                tags: tags.split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
                created_at: from_unix(r.get(3)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // ---- key-value ----

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0)).optional()?)
    }

    pub fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

fn now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn from_unix(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_once() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.schema_version().unwrap(), MIGRATIONS.len());
    }

    #[test]
    fn file_database_persists() {
        let dir = std::env::temp_dir().join(format!("alouette-store-{}", std::process::id()));
        let path = dir.join("state.sqlite3");
        {
            let s = Store::open(&path).unwrap();
            s.kv_set("layout", "tiled").unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.kv_get("layout").unwrap().as_deref(), Some("tiled"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn history_dedupes_by_url_newest_first() {
        let s = Store::in_memory().unwrap();
        s.add_history("about:welcome", "Welcome", "core").unwrap();
        s.add_history("demo:hello", "Hello", "demo").unwrap();
        s.add_history("about:welcome", "Welcome", "core").unwrap();
        let h = s.recent_history(10).unwrap();
        assert_eq!(h.len(), 2);
        s.clear_history().unwrap();
        assert!(s.recent_history(10).unwrap().is_empty());
    }

    #[test]
    fn bookmarks_upsert_and_remove() {
        let s = Store::in_memory().unwrap();
        s.set_bookmark("gopher://gopher.floodgap.com/", "Floodgap", &["gopher"]).unwrap();
        s.set_bookmark("gopher://gopher.floodgap.com/", "Floodgap Systems", &["gopher", "retro"]).unwrap();
        let b = s.bookmarks().unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].title, "Floodgap Systems");
        assert_eq!(b[0].tags, vec!["gopher", "retro"]);
        assert!(s.remove_bookmark("gopher://gopher.floodgap.com/").unwrap());
        assert!(s.bookmarks().unwrap().is_empty());
    }
}
