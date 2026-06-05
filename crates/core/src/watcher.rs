//! Live tail of the projects directory via `notify`.
//!
//! On a filesystem change the watcher reads only the NEW complete lines from
//! each touched `*.jsonl` (using the store's byte offset), inserts the parsed
//! events, advances the offset, and forwards a [`WatchEvent`] over the channel.
//!
//! The notify loop runs on its own thread and owns its own [`Store`] connection
//! (WAL permits concurrent readers/writers). The pure per-file processing step
//! [`process_file_update`] is unit-testable without spinning up notify.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::importer::read_new_complete_lines;
use crate::model::{LineKind, ParsedEvent};
use crate::store::Store;

/// Debounce window for coalescing rapid filesystem events.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// An event delivered to the application from the live watcher.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// New complete lines were read from `file`.
    Events {
        /// The jsonl file that changed.
        file: PathBuf,
        /// Assistant events with usage (for the store / dashboard push).
        events: Vec<ParsedEvent>,
        /// All parsed line kinds (for live pet-state derivation).
        lines: Vec<LineKind>,
    },
}

/// Handle to a running watcher; drop or call [`WatchHandle::stop`] to end it.
pub struct WatchHandle {
    stop_tx: Sender<()>,
    join: Option<JoinHandle<()>>,
    // Keep the watcher alive for the lifetime of the handle.
    _watcher: RecommendedWatcher,
}

impl WatchHandle {
    /// Signal the watcher thread to stop and join it.
    pub fn stop(mut self) {
        let _ = self.stop_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Process a single changed file: read new complete lines from the store's
/// offset, persist them, advance the offset, and forward a [`WatchEvent`] if
/// anything new was read. Idempotent w.r.t. the stored offset.
pub fn process_file_update(path: &Path, store: &Store, tx: &Sender<WatchEvent>) -> Result<()> {
    if path.extension().map(|x| x != "jsonl").unwrap_or(true) {
        return Ok(());
    }
    let key = path.to_string_lossy().to_string();
    let offset = store.get_offset(&key)?;
    let read = read_new_complete_lines(path, offset)?;

    if read.new_offset == offset && read.lines.is_empty() {
        return Ok(()); // nothing new
    }

    if !read.events.is_empty() {
        let batch: Vec<(ParsedEvent, i64)> = read
            .events
            .iter()
            .cloned()
            .map(|e| (e, offset as i64))
            .collect();
        store.insert_batch_at(&batch, &key)?;
    }
    store.set_offset(&key, read.new_offset)?;

    if !read.lines.is_empty() {
        let _ = tx.send(WatchEvent::Events {
            file: path.to_path_buf(),
            events: read.events,
            lines: read.lines,
        });
    }
    Ok(())
}

/// Start watching `projects_dir` recursively. Returns a [`WatchHandle`] that
/// keeps the watcher running until stopped/dropped.
pub fn watch(projects_dir: &Path, store: Store, tx: Sender<WatchEvent>) -> Result<WatchHandle> {
    let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })?;
    watcher.watch(projects_dir, RecursiveMode::Recursive)?;

    let join = std::thread::spawn(move || {
        run_loop(raw_rx, stop_rx, &store, &tx);
    });

    Ok(WatchHandle {
        stop_tx,
        join: Some(join),
        _watcher: watcher,
    })
}

/// The debounce + dispatch loop. Coalesces bursts of events within [`DEBOUNCE`]
/// and processes each touched file once per burst.
fn run_loop(
    raw_rx: mpsc::Receiver<notify::Result<notify::Event>>,
    stop_rx: mpsc::Receiver<()>,
    store: &Store,
    tx: &Sender<WatchEvent>,
) {
    loop {
        if stop_rx.try_recv().is_ok() {
            return;
        }
        // Block for the first event (with a short poll so we can see stop).
        let first = match raw_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ev) => ev,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };

        let mut touched: HashSet<PathBuf> = HashSet::new();
        collect_paths(first, &mut touched);

        // Debounce: keep draining for a short window to coalesce the burst.
        let deadline = std::time::Instant::now() + DEBOUNCE;
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            match raw_rx.recv_timeout(remaining) {
                Ok(ev) => collect_paths(ev, &mut touched),
                Err(_) => break,
            }
        }

        for path in touched {
            let _ = process_file_update(&path, store, tx);
        }
    }
}

/// Extract jsonl paths from a notify event we care about (create/modify).
fn collect_paths(ev: notify::Result<notify::Event>, out: &mut HashSet<PathBuf>) {
    let Ok(ev) = ev else { return };
    if !matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
    ) {
        return;
    }
    for p in ev.paths {
        if p.extension().map(|x| x == "jsonl").unwrap_or(false) {
            out.insert(p);
        }
    }
}
