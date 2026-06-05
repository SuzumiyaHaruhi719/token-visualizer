//! SQLite persistence: schema, idempotent inserts, and import offsets.
//!
//! `events` is the source of truth (one row per assistant message, keyed by
//! `request_id`). `import_state` tracks the byte offset parsed so far per file
//! so re-imports only read new bytes. WAL mode is enabled so the watcher can
//! write while the UI reads.

use std::path::Path;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::model::ParsedEvent;

/// Bump when the `events`/`import_state` schema changes.
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    request_id   TEXT PRIMARY KEY,
    ts           INTEGER NOT NULL,
    session_id   TEXT NOT NULL,
    project      TEXT NOT NULL,
    model        TEXT NOT NULL,
    input        INTEGER NOT NULL,
    output       INTEGER NOT NULL,
    cache_create INTEGER NOT NULL,
    cache_read   INTEGER NOT NULL,
    web_search   INTEGER NOT NULL,
    web_fetch    INTEGER NOT NULL,
    source_file  TEXT,
    line_offset  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_model ON events(model);
CREATE INDEX IF NOT EXISTS idx_events_project ON events(project);
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);

CREATE TABLE IF NOT EXISTS import_state (
    file           TEXT PRIMARY KEY,
    byte_offset    INTEGER NOT NULL,
    schema_version INTEGER NOT NULL
);
"#;

/// A handle to the SQLite database.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a file-backed database with WAL enabled.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init(true)?;
        Ok(store)
    }

    /// Open an in-memory database (for tests). WAL is a no-op for `:memory:`.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init(false)?;
        Ok(store)
    }

    fn init(&self, wal: bool) -> Result<()> {
        if wal {
            // WAL lets the watcher write while the UI reads concurrently.
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.execute_batch(SCHEMA_SQL)?;
        Ok(())
    }

    /// The active SQLite journal mode (e.g. `"wal"`).
    pub fn journal_mode(&self) -> Result<String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode)
    }

    /// Borrow the underlying connection (read-only queries in `query.rs`).
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Insert one event, ignoring duplicates by `request_id`.
    pub fn insert_event(&self, e: &ParsedEvent) -> Result<()> {
        insert_one(&self.conn, e, None, None)?;
        Ok(())
    }

    /// Insert one event recording its source file + byte offset (provenance).
    pub fn insert_event_at(&self, e: &ParsedEvent, source_file: &str, offset: i64) -> Result<()> {
        insert_one(&self.conn, e, Some(source_file), Some(offset))?;
        Ok(())
    }

    /// Insert a batch in a single transaction. Returns the number of NEW rows
    /// (duplicates are ignored). Idempotent across calls.
    pub fn insert_batch(&self, events: &[ParsedEvent]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut inserted = 0usize;
        for e in events {
            inserted += insert_one(&tx, e, None, None)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Insert a batch carrying source-file provenance, in one transaction.
    pub fn insert_batch_at(
        &self,
        events: &[(ParsedEvent, i64)],
        source_file: &str,
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut inserted = 0usize;
        for (e, offset) in events {
            inserted += insert_one(&tx, e, Some(source_file), Some(*offset))?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Total token count (input+output+cache_create+cache_read) across all rows.
    pub fn total_tokens(&self) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(input + output + cache_create + cache_read), 0) FROM events",
            [],
            |row| row.get(0),
        )?;
        Ok(n)
    }

    /// Number of event rows.
    pub fn event_count(&self) -> Result<i64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        Ok(n)
    }

    /// Stored byte offset for a file, or 0 if never recorded.
    pub fn get_offset(&self, file: &str) -> Result<u64> {
        let off: Option<i64> = self
            .conn
            .query_row(
                "SELECT byte_offset FROM import_state WHERE file = ?1",
                params![file],
                |row| row.get(0),
            )
            .ok();
        Ok(off.unwrap_or(0).max(0) as u64)
    }

    /// Record the byte offset parsed so far for a file.
    pub fn set_offset(&self, file: &str, offset: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO import_state(file, byte_offset, schema_version) VALUES (?1, ?2, ?3)
             ON CONFLICT(file) DO UPDATE SET byte_offset = excluded.byte_offset,
                                             schema_version = excluded.schema_version",
            params![file, offset as i64, SCHEMA_VERSION],
        )?;
        Ok(())
    }
}

/// Insert a single event with `INSERT OR IGNORE`. Returns 1 if a row was added,
/// 0 if it was a duplicate.
fn insert_one(
    conn: &Connection,
    e: &ParsedEvent,
    source_file: Option<&str>,
    offset: Option<i64>,
) -> Result<usize> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO events
            (request_id, ts, session_id, project, model,
             input, output, cache_create, cache_read, web_search, web_fetch,
             source_file, line_offset)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            e.request_id,
            e.ts,
            e.session_id,
            e.project,
            e.model,
            e.usage.input,
            e.usage.output,
            e.usage.cache_create,
            e.usage.cache_read,
            e.usage.web_search,
            e.usage.web_fetch,
            source_file,
            offset,
        ],
    )?;
    Ok(changed)
}
