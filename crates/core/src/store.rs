//! SQLite persistence: schema, idempotent inserts, and import offsets.
//!
//! `events` is the source of truth (one row per assistant message, keyed by
//! `request_id`). `import_state` tracks the byte offset parsed so far per file
//! so re-imports only read new bytes. WAL mode is enabled so the watcher can
//! write while the UI reads.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::model::ParsedEvent;

/// Bump when the `events`/`import_state` schema changes.
pub const SCHEMA_VERSION: i64 = 2;

/// How long a connection waits for the write lock before returning SQLITE_BUSY.
/// Sized to outlast a worst-case backfill write burst so readers never fail.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

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
    source       TEXT NOT NULL DEFAULT 'claude',
    source_file  TEXT,
    line_offset  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts);
CREATE INDEX IF NOT EXISTS idx_events_model ON events(model);
CREATE INDEX IF NOT EXISTS idx_events_project ON events(project);
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
-- NOTE: the index on `source` is created in `migrate()`, NOT here. On a
-- pre-existing v1 `events` table the CREATE TABLE above is a no-op (so the
-- `source` column is still absent), and creating the index here would fail
-- with "no such column: source" before migrate() can add the column.

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
        // Wait (rather than fail with SQLITE_BUSY) when another connection holds
        // the write lock. The startup backfill can hold it for several seconds;
        // without this, every concurrent reader/opener — including the schema
        // DDL below — would error out instantly while the import runs.
        self.conn.busy_timeout(BUSY_TIMEOUT)?;
        if wal {
            // WAL lets the watcher write while the UI reads concurrently.
            self.conn.pragma_update(None, "journal_mode", "WAL")?;
        }
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.migrate()?;
        self.repair_data()?;
        Ok(())
    }

    /// Apply forward-only migrations to a pre-existing `events` table. Adding the
    /// `source` column to a v1 DB is idempotent: if it already exists (fresh DB
    /// from `SCHEMA_SQL`) the `ALTER` errors and is ignored.
    fn migrate(&self) -> Result<()> {
        let has_source = self
            .conn
            .prepare("SELECT source FROM events LIMIT 0")
            .is_ok();
        if !has_source {
            // Add the column to a pre-existing v1 table; existing rows default
            // to 'claude'. (A fresh DB already has it from SCHEMA_SQL.)
            self.conn
                .execute_batch("ALTER TABLE events ADD COLUMN source TEXT NOT NULL DEFAULT 'claude';")?;
        }
        // The `source` column now exists either way, so it is safe to index it.
        self.conn
            .execute_batch("CREATE INDEX IF NOT EXISTS idx_events_source ON events(source);")?;
        Ok(())
    }

    /// One-time, forward-only DATA repairs keyed on `PRAGMA user_version` (an
    /// integer in the DB header). Distinct from [`Self::migrate`], which fixes
    /// the schema — this fixes already-imported rows. Each repair runs at most
    /// once per database and is a no-op on a fresh one.
    fn repair_data(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;

        // Repair 1: before the incremental-read fix, Codex tail reads attributed
        // live `token_count` turns to model "" (surfaced as the "other" bucket),
        // because the model — declared once near the top of a rollout — was not
        // rebuilt when reading from a stored offset. Drop those misattributed
        // rows and reset the Codex import offsets so the next backfill re-imports
        // every rollout from the start with the correct model. Safe: Codex events
        // are fully regenerable from the read-only rollout logs.
        if version < 1 {
            self.conn.execute_batch(
                "DELETE FROM events WHERE source = 'codex' AND model = '';
                 DELETE FROM import_state WHERE file LIKE 'codex:%';",
            )?;
            self.conn.pragma_update(None, "user_version", 1i64)?;
        }

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
             source, source_file, line_offset)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            e.source.as_str(),
            source_file,
            offset,
        ],
    )?;
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-`source` (v1) `events` schema: no `source`/`source_file`/`line_offset`
    /// columns. Mirrors a DB created before the Codex/source migration shipped.
    const V1_SCHEMA_SQL: &str = r#"
        CREATE TABLE events (
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
            web_fetch    INTEGER NOT NULL
        );
        CREATE TABLE import_state (
            file           TEXT PRIMARY KEY,
            byte_offset    INTEGER NOT NULL,
            schema_version INTEGER NOT NULL
        );
    "#;

    fn temp_db_path() -> std::path::PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cm-store-test-{}-{now}.sqlite", std::process::id()))
    }

    /// Regression: opening a pre-existing v1 DB (events table lacking `source`)
    /// must upgrade cleanly instead of failing with "no such column: source".
    /// This reproduces the bug where the source index lived in SCHEMA_SQL and
    /// ran against the old table before migrate() added the column.
    #[test]
    fn opens_and_upgrades_a_v1_database() {
        let path = temp_db_path();

        // Build a v1 DB with one row, then drop the connection.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V1_SCHEMA_SQL).unwrap();
            conn.execute(
                "INSERT INTO events
                    (request_id, ts, session_id, project, model,
                     input, output, cache_create, cache_read, web_search, web_fetch)
                 VALUES ('r1', 1, 's1', 'p', 'claude-opus-4-8', 10, 20, 0, 0, 0, 0)",
                [],
            )
            .unwrap();
        }

        // The previously-broken path: opening must succeed (runs SCHEMA_SQL then migrate).
        let store = Store::open(&path).expect("v1 DB should upgrade, not error");

        // `source` column now exists and the legacy row defaulted to 'claude'.
        let src: String = store
            .conn
            .query_row("SELECT source FROM events WHERE request_id = 'r1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(src, "claude");

        // The source index was created by migrate(), not SCHEMA_SQL.
        let idx_count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='index' AND name='idx_events_source'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1);

        // Idempotent: opening again is a no-op, not an error.
        let _ = Store::open(&path).expect("second open should also succeed");

        let _ = std::fs::remove_file(&path);
    }

    /// Repair #1: a DB carrying Codex rows misattributed to model "" (the
    /// pre-fix "other" bucket) is cleaned on open — the bad rows are dropped and
    /// the Codex offsets reset so the next backfill re-imports them — while the
    /// correctly-attributed Codex rows and all Claude rows are kept. The repair
    /// is keyed on `user_version`, so it runs once and never again.
    #[test]
    fn repairs_codex_empty_model_attribution_once() {
        let path = temp_db_path();

        let seed_event = |conn: &Connection, id: &str, model: &str, source: &str| {
            conn.execute(
                "INSERT INTO events
                    (request_id, ts, session_id, project, model,
                     input, output, cache_create, cache_read, web_search, web_fetch, source)
                 VALUES (?1, 1, 's', 'p', ?2, 100, 0, 0, 0, 0, 0, ?3)",
                params![id, model, source],
            )
            .unwrap();
        };

        // A DB created before the repair shipped: user_version 0, seeded with a
        // misattributed Codex row, a good Codex row, a Claude row, and a Codex
        // import offset that would otherwise skip re-import.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_SQL).unwrap();
            conn.pragma_update(None, "user_version", 0i64).unwrap();
            seed_event(&conn, "cx-bad", "", "codex");
            seed_event(&conn, "cx-good", "gpt-5.5", "codex");
            seed_event(&conn, "cc", "claude-opus-4-8", "claude");
            conn.execute(
                "INSERT INTO import_state(file, byte_offset, schema_version)
                 VALUES ('codex:/x/rollout.jsonl', 4096, 2)",
                [],
            )
            .unwrap();
        }

        // Opening runs repair_data().
        let store = Store::open(&path).unwrap();
        let count = |sql: &str| -> i64 { store.conn.query_row(sql, [], |r| r.get(0)).unwrap() };

        assert_eq!(
            count("SELECT count(*) FROM events WHERE source='codex' AND model=''"),
            0,
            "misattributed Codex rows dropped"
        );
        assert_eq!(
            count("SELECT count(*) FROM events WHERE source='codex' AND model='gpt-5.5'"),
            1,
            "correctly-attributed Codex rows kept"
        );
        assert_eq!(
            count("SELECT count(*) FROM events WHERE source='claude'"),
            1,
            "Claude rows untouched"
        );
        assert_eq!(
            store.get_offset("codex:/x/rollout.jsonl").unwrap(),
            0,
            "Codex offset reset so backfill re-imports"
        );
        let uv: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 1, "repair marked done");

        // One-shot: a fresh misattributed row inserted after the repair must
        // survive a re-open (the gate prevents the repair from running again).
        seed_event(&store.conn, "cx-bad2", "", "codex");
        drop(store);
        let store2 = Store::open(&path).unwrap();
        let bad2: i64 = store2
            .conn
            .query_row(
                "SELECT count(*) FROM events WHERE request_id='cx-bad2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bad2, 1, "repair is one-shot, not run on every open");

        let _ = std::fs::remove_file(&path);
    }

    /// A brand-new DB opens cleanly and already has the `source` column + index.
    #[test]
    fn opens_a_fresh_database_with_source() {
        let path = temp_db_path();
        let store = Store::open(&path).unwrap();
        assert!(store
            .conn
            .prepare("SELECT source FROM events LIMIT 0")
            .is_ok());
        let _ = std::fs::remove_file(&path);
    }
}
