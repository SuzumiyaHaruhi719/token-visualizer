//! Locating Claude Code's data (read-only) and the app's own data dir.
//!
//! Two distinct roots:
//! * `claude_home()` / `projects_dir()` / `sessions_dir()` point at the
//!   *source* logs under `~/.claude`. The app only ever READS these.
//! * `app_data_dir()` / `default_db_path()` point at the app's OWN writable
//!   data dir (e.g. `%APPDATA%/claude-monitor`). All persistence goes here.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

/// The application data directory name (under the platform data dir).
pub const APP_DIR_NAME: &str = "claude-monitor";

/// `~/.claude` — the root of Claude Code's data. Read-only.
pub fn claude_home() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    Ok(home.join(".claude"))
}

/// `~/.claude/projects` — where the jsonl session logs live. Read-only.
pub fn projects_dir() -> Result<PathBuf> {
    Ok(claude_home()?.join("projects"))
}

/// `~/.claude/sessions` — per-pid live status json. Read-only.
pub fn sessions_dir() -> Result<PathBuf> {
    Ok(claude_home()?.join("sessions"))
}

/// The app's own writable data directory, e.g. `%APPDATA%/claude-monitor`.
///
/// Falls back to the home dir if the platform data dir is unavailable, but
/// NEVER resolves under `~/.claude`.
pub fn app_data_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .or_else(dirs::config_dir)
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow!("could not resolve a data directory"))?;
    Ok(base.join(APP_DIR_NAME))
}

/// Default SQLite database path: `<app_data_dir>/db.sqlite`.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("db.sqlite"))
}

/// Default editable price-table path: `<app_data_dir>/pricing.json`.
pub fn default_pricing_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("pricing.json"))
}

/// Friendly project name from a `cwd` string: the last non-empty path segment,
/// splitting on both `/` and `\`. Falls back to `"unknown"` when empty.
pub fn project_name_from_cwd(cwd: &str) -> String {
    cwd.split(['/', '\\'])
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}
