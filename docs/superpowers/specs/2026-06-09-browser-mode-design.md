# Browser mode — design

## Goal

Run the **full** Claude Monitor dashboard in a regular web browser — cross-platform
(macOS / Windows / Linux) — by running one command or double-clicking a script.
No Tauri app, window, or tray. **Every desktop animation is preserved** (it's the
same frontend bundle). Pet/tray are desktop-only and simply absent.

## Why this is small

The dashboard is already a web app: the embedded axum server serves `dist/` +
the JSON API + SSE, and the frontend talks to it over HTTP/SSE. Every Tauri-only
feature (window drag, F11, popover IPC) is already guarded to no-op when there's
no Tauri IPC. So browser mode = **run that server headless and open a browser at
it.** The frontend needs ~zero changes, so the continuous token roll, ECharts,
odometer reels, KPI/limits transitions, currency, etc. all work identically.

## Approach (A — chosen)

Extract the non-GUI bootstrap out of the Tauri `setup` closure into one reusable
`run_core(...)`; the Tauri app calls it and adds the desktop layer, while a new
headless **`cm-serve`** workspace binary calls *only* `run_core`. (Rejected: a
`--serve` flag on the Tauri binary — still links Tauri + wants an event loop;
and a Node reimplementation — duplicates the whole Rust ingestion/SQLite layer.)

## Components

1. **`run_core(...)`** — shared core runtime. Does: load settings + prices, open
   the store, resolve `dist/`, bind axum, spawn backfill (Claude + Codex), spawn
   the file watchers (Claude + Codex), spawn the session-poll loop in a
   **headless** mode that PUBLISHES the `sessions`/`usage` SSE (the dashboard's
   live strip + roll need it) but performs NO desktop side-effects (no tray
   tooltip, no popover). Spawn Discord only if configured. Returns the bound port
   (or serves/blocks). The Tauri `setup` is refactored to call `run_core` then
   create the dashboard window + tray.
   - `state_poll` is split: the publish path stays; `drive_desktop` (tray
     tooltip) becomes a desktop-only callback the headless path skips.
2. **`cm-serve` binary** — new workspace bin, NO Tauri dependency (fast build,
   compiles identically on all OSes). Reads port from env `CM_PORT` (default
   `8788`), calls `run_core`, prints the URL, and **auto-opens the default
   browser** cross-platform via a small crate (`open`/`webbrowser`: `open` on
   macOS, `start` on Windows, `xdg-open` on Linux). Then blocks forever.
   - Must locate `dist/` without Tauri's resource dir: resolve relative to the
     binary / `CARGO_MANIFEST_DIR` / cwd, with an env override (`CM_DIST`).
3. **Launch, cross-platform:** `npm run serve` (npm runs on all OSes) →
   `cargo run -p cm-serve --release` (build-if-needed + run). Plus thin
   double-click launchers: `serve.command` (macOS), `serve.sh` (Linux/macOS),
   `serve.bat` (Windows) — each just invokes the same.
4. **Frontend:** no changes that touch animations. It already runs same-origin in
   a browser (no `__CM_PORT__` needed) and no-ops Tauri features. Desktop-only
   settings toggles (tray monitor / notifications / sound) are harmless no-ops in
   a browser; **leave the frontend as-is for v1** to guarantee zero animation/test
   regressions (optionally hide those rows later behind a no-Tauri check — must
   not alter any animation).
5. **Fixed port** `8788` (env override) so the URL is stable/bookmarkable.

## Data

Same `~/.claude` + `~/.codex` (read-only) and the same SQLite store / pricing /
fx under the cross-platform app-data dir (`dirs`). SQLite WAL permits concurrent
processes, but running the app *or* browser mode (not both) is recommended.

## Testing

- A smoke test that boots `cm-serve` on a test port and asserts `GET /` → 200
  (index.html) and `GET /api/summary?range=all` → JSON.
- All existing cargo + frontend tests stay green.
- Manual: `npm run serve` opens the browser to the live dashboard with all
  animations running (roll, charts, transitions).

## Cross-platform notes

- Rust + the browser-open crate + npm scripts are all cross-platform; paths use
  `dirs`. macOS gets a double-clickable `serve.command`.

## Out of scope

Pet/tray (desktop-only); shipping a prebuilt binary (build-from-source via the
script); any change that removes or degrades a dashboard animation.
