# Claude Monitor 🦀

A lightweight Windows desktop app that gives you **full, persistent, real-time visibility into your Claude Code token usage** — plus **Clawd desktop pets** that animate to reflect what Claude is doing right now.

It reads Claude Code's own logs (**read-only**), stores everything in a local SQLite database, and shows three surfaces:

- **System-tray widget** — glance at the current session's live usage.
- **Web dashboard** — charts and multi-dimensional breakdowns (by model / project / session / day / cache type), full history, cost estimate. Opens in the app window *or* any browser at the local URL.
- **Clawd desktop pets** — one transparent, always-on-top pixel pet **per active Claude Code session**, labeled by project, with per-part animations for each work state: 😌 idle · 🤔 thinking · 🔧 working (shows the tool) · 💬 responding · ⏳ waiting · 💤 sleeping. A pet plays a leave animation when its session ends.

## Architecture

```
~/.claude/projects/**/*.jsonl  (token usage + events)   ──┐  read-only
~/.claude/sessions/<pid>.json  (live busy/idle status)  ──┤
                                                          ▼
   crates/core  (Rust lib: parser · pricing · store · query · state · importer · watcher)
                                                          ▼
   src-tauri    (Tauri 2 app: SQLite (WAL) + axum localhost server + SSE + tray + windows)
                                                          ▼
   src/         (frontend: dashboard via ECharts + Clawd pet as a 12×8 SVG part-rig)
```

The core logic is a pure, fully unit-tested Rust crate (`claude-monitor-core`). The Tauri app runs an embedded axum server on `127.0.0.1` that serves the built frontend **and** a JSON/SSE API; all windows (and any browser) talk to that one local server.

## Requirements

- Windows 10/11 with **WebView2 runtime** (ships with Win11; else `winget install Microsoft.EdgeWebView2Runtime`)
- **Rust** (stable, MSVC) + VS Build Tools, **Node** 20+ / npm
- Note: on this machine the Rust toolchain lives in `~/.cargo/bin`, which may not be on the shell PATH — prefix cargo commands with `export PATH="$HOME/.cargo/bin:$PATH"` if `cargo` isn't found.

## Run (development)

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # if needed
npm install
npm run tauri dev
```

## Build (release installer)

```bash
export PATH="$HOME/.cargo/bin:$PATH"
npm run tauri build
# → installer under src-tauri/target/release/bundle/ (MSI/NSIS)
```

## Data & configuration

- Database: `%APPDATA%\claude-monitor\db.sqlite` (your usage history; survives restarts).
- Editable price table: `%APPDATA%\claude-monitor\pricing.json` — seeded with public Anthropic API list prices (input/output; cache-write = 1.25× input, cache-read = 0.10× input). Cost is shown as an **estimate**; edit this file to correct or localize prices. Unknown models show cost as “—”.

## Tests

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p claude-monitor-core   # core logic (parser/pricing/store/query/state/importer/watcher)
npm test                            # frontend (format, pet state mapping, grid integrity, render)
```

## Notes

- **Strictly read-only** on `~/.claude` — the app never writes to or modifies Claude Code's files.
- **Proxy/localhost:** if a system HTTP(S) proxy is set (e.g. Clash) with an empty no-proxy list it can hang localhost; the app bypasses the proxy for `127.0.0.1`/`localhost`. If the dashboard ever hangs blank, check your proxy's no-proxy list.
- **Clawd** is Anthropic's mascot/IP. This is a personal, unpublished tool; redistribution would require Anthropic's permission.
