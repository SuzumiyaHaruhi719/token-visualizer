//! Backfill: walk `*.jsonl` recursively and stream new bytes into the store.
//!
//! Streaming + offset tracking means re-running is cheap and idempotent: each
//! file is read only from its stored byte offset, and only COMPLETE lines (those
//! terminated by `\n`) are consumed. A half-written trailing line does not
//! advance the offset, so it is re-read once completed (design §8).

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use anyhow::Result;
use walkdir::WalkDir;

use crate::model::{LineKind, ParsedEvent};
use crate::parser::parse_line;
use crate::store::Store;

/// Result of reading new complete lines from a file.
pub struct IncrementalRead {
    /// Byte offset where this read actually started. This differs from the
    /// requested offset when a file has shrunk and must be re-read from zero.
    pub start_offset: u64,
    /// Assistant events parsed from the newly-read complete lines.
    pub events: Vec<ParsedEvent>,
    /// All line kinds parsed (for live state derivation by the watcher).
    pub lines: Vec<LineKind>,
    /// Byte offset after the last COMPLETE line; persist this.
    pub new_offset: u64,
}

/// Walk `projects_dir` for `*.jsonl` files and import any new complete lines.
///
/// `progress(done, total)` is called after each file is processed.
pub fn backfill(
    projects_dir: &Path,
    store: &Store,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    let files = collect_jsonl(projects_dir);
    let total = files.len();

    for (idx, path) in files.iter().enumerate() {
        let key = path.to_string_lossy().to_string();
        let offset = store.get_offset(&key)?;
        let read = read_new_complete_lines(path, offset)?;

        if !read.events.is_empty() {
            // Record provenance (source_file, line_offset == start offset).
            let start_offset = read.start_offset as i64;
            let batch: Vec<(ParsedEvent, i64)> =
                read.events.into_iter().map(|e| (e, start_offset)).collect();
            store.insert_batch_at(&batch, &key)?;
        }
        store.set_offset(&key, read.new_offset)?;

        progress(idx + 1, total);
    }
    Ok(())
}

/// Recursively collect `*.jsonl` files under `dir`, sorted for determinism.
fn collect_jsonl(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Read from `offset` to EOF, returning parsed events + the new offset (end of
/// the last complete, newline-terminated line). Streams line-by-line; never
/// loads the whole file.
pub fn read_new_complete_lines(path: &Path, offset: u64) -> Result<IncrementalRead> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = if offset > len { 0 } else { offset };
    if start == len {
        return Ok(IncrementalRead {
            start_offset: start,
            events: Vec::new(),
            lines: Vec::new(),
            new_offset: start,
        });
    }
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);

    let mut events = Vec::new();
    let mut lines = Vec::new();
    let mut consumed = start;
    let mut buf = Vec::new();

    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF
        }
        // Only consume the line if it is newline-terminated (complete).
        if buf.last() != Some(&b'\n') {
            // Half-written trailing line: stop WITHOUT advancing past it.
            break;
        }
        consumed += n as u64;
        let text = String::from_utf8_lossy(&buf);
        let kind = parse_line(&text);
        if let LineKind::Assistant(ref e) = kind {
            events.push(e.clone());
        }
        lines.push(kind);
    }

    Ok(IncrementalRead {
        start_offset: start,
        events,
        lines,
        new_offset: consumed,
    })
}
