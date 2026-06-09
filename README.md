# Claude Monitor 🦀

A lightweight Windows desktop app that gives you **full, persistent, real-time visibility into your Claude Code token usage**.

It reads Claude Code's own logs (**read-only**), stores everything in a local SQLite database, and shows two surfaces:

- **System-tray widget** — glance at the current session's live usage.
- **Web dashboard** — charts and multi-dimensional breakdowns (by model / project / session / day / cache type), full history, cost estimate, plus a live **session strip** that shows each active session's work state (😌 idle · 🤔 thinking · 🔧 working, with the tool · 💬 responding · ⏳ waiting · 💤 sleeping). Opens in the app window *or* any browser at the local URL.

## Architecture

```
~/.claude/projects/**/*.jsonl  (token usage + events)   ──┐  read-only
~/.claude/sessions/<pid>.json  (live busy/idle status)  ──┤
~/.codex/sessions/**/*.jsonl   (Codex usage + limits)   ──┤
                                                          ▼
   crates/core    (Rust lib: parser · pricing · store · query · state · importer · watcher · codex)
                                                          ▼
   crates/server  (Rust lib `cmserver`: GUI-free core runtime — axum localhost
                   server + SSE + SQLite (WAL) ingestion + fx + settings +
                   discord + headless session-poll; `run_core`)
                          ├──────────────────────────┐
                          ▼                           ▼
   src-tauri (desktop)    crates/cm-serve (browser mode `cm-serve`):
   tray + window +        calls run_core, opens your default browser, blocks.
   popover + notify       No tray/window/pet.
                                                          ▼
   src/           (frontend: dashboard via ECharts + live session strip)
```

The core logic is a pure, fully unit-tested Rust crate (`claude-monitor-core`). The
GUI-free **`cmserver`** crate (`crates/server`) wraps it in `run_core(...)`: an
embedded axum server on `127.0.0.1` that serves the built frontend **and** a JSON/SSE
API, plus all ingestion/watchers/fx. Two front ends share `run_core` verbatim — the
**Tauri desktop app** (`src-tauri`, which adds the tray/window/popover/notifications)
and the headless **`cm-serve`** binary (`crates/cm-serve`, browser mode). Because both
serve the identical frontend bundle, the dashboard (continuous token roll, ECharts,
odometer reels, KPI/limits transitions, currency) behaves the same in a browser tab as
in the desktop window; only the tray/popover/pet are desktop-only.

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

## Browser mode (no desktop app) 🌐

Run the **full dashboard in a normal web browser** — cross-platform (macOS / Windows /
Linux), no Tauri window, tray, or pet. It's the same frontend bundle and the same local
server, so every animation (continuous token roll, charts, odometer reels, limits
countdowns, currency) works identically.

**One command** (any OS, needs Node + Rust):

```bash
npm run serve          # = cargo run -p cm-serve --release; builds if needed, opens your browser
```

**Or double-click a launcher** (each builds if needed, then opens your browser):

| OS | File | First-time note |
|----|------|-----------------|
| macOS | `serve.command` | `chmod +x serve.command` once (or right-click → Open to clear Gatekeeper) |
| Linux / macOS | `serve.sh` | `chmod +x serve.sh` once |
| Windows | `serve.bat` | just double-click |

It serves a **stable, bookmarkable** URL (default `http://127.0.0.1:8788/`) and opens your
default browser there. Environment overrides:

- `CM_PORT` — port to bind (default `8788`), e.g. `CM_PORT=9000 npm run serve`.
- `CM_DIST` — explicit path to the built frontend `dist/` (otherwise resolved relative to
  the binary / workspace / cwd).
- `CM_NO_OPEN=1` — don't auto-open a browser (headless servers, scripting).

Notes:
- The frontend `dist/` is built automatically by the launchers if missing (via `npm run build`).
- If the desktop app is already running, `cm-serve` detects it and just opens your browser
  at the existing instance instead of starting a second copy. Run the app **or** browser
  mode (they share the same SQLite store; running both writers is not recommended).

## Data & configuration

- Database: `%APPDATA%\claude-monitor\db.sqlite` (your usage history; survives restarts).
- Editable price table: `%APPDATA%\claude-monitor\pricing.json` — seeded with public Anthropic API list prices (input/output; cache-write = 1.25× input, cache-read = 0.10× input). Cost is shown as an **estimate**; edit this file to correct or localize prices. Unknown models show cost as “—”.

## Tests

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace              # core logic + server (run_core/axum/fx/settings) + cm-serve smoke test
npm test                            # frontend (format, session-state mapping, dashboard, settings)
```

The `cm-serve` smoke test boots the real server on an ephemeral port and asserts
`GET /` → 200 (index.html) and `GET /api/summary` → JSON.

## Notes

- **Strictly read-only** on `~/.claude` — the app never writes to or modifies Claude Code's files.
- **Proxy/localhost:** if a system HTTP(S) proxy is set (e.g. Clash) with an empty no-proxy list it can hang localhost; the app bypasses the proxy for `127.0.0.1`/`localhost`. If the dashboard ever hangs blank, check your proxy's no-proxy list.
- **Clawd** is Anthropic's mascot/IP. This is a personal, unpublished tool; redistribution would require Anthropic's permission.
