//! `claude-monitor-core` — pure logic for the Claude Monitor desktop app.
//!
//! Parses Claude Code's own jsonl session logs, persists token usage to SQLite,
//! derives per-session "pet" work-state, and produces dashboard aggregations.
//! Strictly read-only on `~/.claude`; all persistence goes to the app's own
//! data directory.

pub mod importer;
pub mod model;
pub mod parser;
pub mod paths;
pub mod pricing;
pub mod query;
pub mod state;
pub mod store;
pub mod watcher;

pub use model::{
    LineKind, ModelBreakdown, ParsedEvent, PetState, ProjectBreakdown, SessionState, Summary,
    TimeseriesBucket, Totals, Usage,
};
