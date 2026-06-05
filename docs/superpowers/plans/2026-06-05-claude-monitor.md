# Claude Monitor + Clawd Pet — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Windows desktop app (Tauri 2) that reads Claude Code's own logs, persists token usage to SQLite, and shows a tray widget, a web dashboard, and per-session animated Clawd desktop pets reflecting Claude's live work state.

**Architecture:** A Cargo workspace with a pure-logic `core` crate (parse / store / import / watch / state / query / pricing — fully unit-testable without any GUI) and a `src-tauri` app crate that runs an embedded **axum** server on `127.0.0.1` (serves the built Svelte frontend + JSON API + SSE) and owns the desktop shell (system tray, dashboard window, and one transparent always-on-top **pet** window per active session). All UI (Tauri webview *and* a real browser) talks to the same localhost server, so the frontend and backend can be built in parallel against a fixed HTTP/SSE contract. Strictly read-only on `~/.claude`.

**Tech Stack:** Rust 1.96 (MSVC), Tauri 2, rusqlite (bundled, WAL), notify 6, tokio + axum 0.7, serde/serde_json; Svelte 5 + TypeScript + Vite + ECharts. Node 24 / npm 11.

**Spec:** `docs/superpowers/specs/2026-06-05-claude-monitor-design.md` (§1–§12).

---

## Environment preconditions (this machine — verified 2026-06-05)

- `~/.cargo/bin` is **NOT** on the Bash-tool PATH. Every cargo/rust command MUST start with:
  `export PATH="$HOME/.cargo/bin:$PATH"`
- cargo/rustc 1.96 `stable-x86_64-pc-windows-msvc`; VS BuildTools 2022 (VC 14.44); WinSDK 10.0.26100; Node v24.14.1; npm 11. Trivial `cargo build` links with **no vcvars** needed.
- **Clash/localhost hazard** (see memory `clash-proxy-breaks-localhost`): a system HTTP(S)_PROXY with empty NO_PROXY can hang localhost. Frontend fetches MUST hit `127.0.0.1` (not `localhost`) and the app should set `NO_PROXY=127.0.0.1,localhost` for any of its own HTTP. Document in README.
- Real data to test against: `~/.claude/projects/**/*.jsonl` (551 files, ~646MB), `~/.claude/sessions/<pid>.json` (live status).

## Read-only invariant (CRITICAL, applies to every task)

The app NEVER opens any path under `~/.claude` for writing. All persistence goes to the app's own data dir (`%APPDATA%/claude-monitor/` → `db.sqlite`, `pricing.json`, `pet-positions.json`). Add a unit test asserting the store path is under the app data dir, not under `.claude`.

---

## File Structure

```
claude-monitor/
  Cargo.toml                      # [workspace] members = ["crates/core","src-tauri"]
  rust-toolchain.toml             # pin stable
  crates/core/
    Cargo.toml
    src/lib.rs                    # re-exports; pub mod model/parser/...
    src/model.rs                  # Usage, ParsedEvent, LineKind, PetState, DTOs
    src/paths.rs                  # claude_home(), projects_dir(), sessions_dir(), project_name_from_cwd()
    src/parser.rs                 # parse_line(&str) -> LineKind
    src/pricing.rs                # PriceTable, cost_usd(usage, model)
    src/store.rs                  # Store: schema, insert_event (dedup), import-offset, queries
    src/importer.rs               # backfill(dir, &Store, progress_cb)
    src/watcher.rs                # watch(projects_dir, tx) live tail (incremental by offset)
    src/state.rs                  # derive_pet_states(sessions_dir, &Store|tails) -> Vec<SessionState>
    src/query.rs                  # summary/breakdowns/timeseries/current/sessions DTOs
    tests/                        # parser_tests, pricing_tests, state_tests, store_tests, import_tests
    tests/fixtures/               # tiny jsonl + sessions json samples
  src-tauri/
    Cargo.toml                    # depends on core; tauri, axum, tokio, tower-http
    tauri.conf.json
    build.rs
    src/main.rs                   # bootstrap: app data dir, Store, spawn importer+watcher+state, axum, tray, windows
    src/server.rs                 # axum: static assets + /api/* + /events (SSE broadcast)
    src/windows.rs                # dashboard window; pet window spawn/despawn keyed by session; position memory
    src/tray.rs                   # tray icon + menu + live summary tooltip
    icons/                        # tray + app icons (generated)
  index.html                      # dashboard entry (Vite)
  pet.html                        # pet entry (Vite) — ?session=<id>
  src/                            # frontend
    main.ts                       # dashboard bootstrap
    lib/api.ts                    # fetch helpers (127.0.0.1) + SSE client
    lib/format.ts                 # number/token/cost formatting
    Dashboard.svelte              # KPIs + charts + current session
    components/*.svelte           # KpiCard, TokenTimeseries, ModelDonut, ProjectBars, CachePanel, CurrentSession
    pet/pet-main.ts               # pet bootstrap (reads ?session, subscribes SSE)
    pet/Clawd.svelte              # the 12×8 rig (parts) + per-state animation classes
    pet/clawd-grid.ts             # the extracted 12×8 grid + part map (single source of truth)
  package.json, vite.config.ts, svelte.config.js, tsconfig.json
  assets/clawd/clawd-base.png     # official reference sprite (already committed)
```

---

## Pinned contracts (build everything against these)

### Rust core types (`crates/core/src/model.rs`)

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_create: i64,
    pub cache_read: i64,
    pub web_search: i64,
    pub web_fetch: i64,
}
impl Usage { pub fn total(&self) -> i64 { self.input + self.output + self.cache_create + self.cache_read } }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedEvent {
    pub request_id: String,   // dedup key (fallback to uuid if absent)
    pub ts: i64,              // epoch millis, UTC
    pub session_id: String,
    pub project: String,      // friendly name from cwd basename
    pub model: String,
    pub usage: Usage,
}

/// What a single jsonl line represents (only the variants we use).
#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    Assistant(ParsedEvent),                 // has message.usage
    Thinking,                               // assistant thinking block
    ToolUse { id: String, name: String },   // tool call started
    ToolResult { tool_use_id: String },     // tool finished
    EndTurn,                                // stop_reason == end_turn
    Other,                                  // user/system/etc — ignored for usage
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "tool", rename_all = "snake_case")]
pub enum PetState {
    Idle,
    Thinking,
    Working(Option<String>), // tool name
    Responding,
    Waiting,
    Sleeping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub state: PetState,
    pub tokens: i64,         // running total for this session
    pub updated_at: i64,
}
```

### HTTP/SSE contract (axum, served on `http://127.0.0.1:<port>`)

- `GET /` → dashboard (static `index.html` + assets)
- `GET /pet` → pet page (static `pet.html`)
- `GET /api/summary?range=today|7d|30d|all` → `Summary`
- `GET /api/current` → `SessionState | null` (most-recent active session)
- `GET /api/sessions` → `SessionState[]` (all active sessions — drives pets)
- `GET /api/pricing` (GET/PUT) → editable price table
- `GET /events` → **SSE**, `event:` is one of:
  - `usage` data `Summary`-delta or `{current: SessionState}` (push on new events)
  - `sessions` data `SessionState[]` (push on any session add/remove/state-change)
  - `import` data `{done:int,total:int}` (backfill progress)

```ts
// src/lib/api.ts — TS mirror of the DTOs
export interface Totals { tokens:number; input:number; output:number; cacheCreate:number; cacheRead:number; costUsd:number|null; cacheHitRate:number; messages:number; sessions:number; }
export interface Summary { range:string; totals:Totals; byModel:{model:string;tokens:number;costUsd:number|null}[]; byProject:{project:string;tokens:number}[]; timeseries:{bucket:string;input:number;output:number;cacheCreate:number;cacheRead:number}[]; }
export type PetState = {kind:'idle'|'thinking'|'responding'|'waiting'|'sleeping'} | {kind:'working';tool:string|null};
export interface SessionState { sessionId:string; project:string; model:string; state:PetState; tokens:number; updatedAt:number; }
```

> Server returns camelCase JSON (serde `rename_all="camelCase"` on DTO structs). Pin this now so both sides agree.

### Clawd grid (single source of truth, `src/pet/clawd-grid.ts`)

```ts
// 12 cols × 8 rows, extracted 1:1 from assets/clawd/clawd-base.png
export const GRID = [
  "..OOOOOOOO..",
  "..OKOOOOKO..",
  "OOOOOOOOOOOO",
  "OOOOOOOOOOOO",
  "..OOOOOOOO..",
  "..OOOOOOOO..",
  "..O.O..O.O..",
  "..O.O..O.O..",
];
export const ORANGE = "#da7757";
export const EYE = "#171717";
// part map: arms = cols{0,1}&{10,11} on rows 2-3; legs = cols{2,4,7,9} on rows 6-7; eyes = 'K'; else body
```

---

## Phase 0 — Scaffold + toolchain gate

### Task 0.1: Initialize Tauri + Svelte-TS project into the existing repo

**Files:** workspace root, `src-tauri/`, `src/`, `package.json`, `index.html`, configs.

- [ ] **Step 1:** From repo root, scaffold via the official template into a temp dir, then move files in (keeps `docs/`, `assets/`, `.git`).
```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /c/Users/Thomas/Documents/Projects
npm create tauri-app@latest cm-scaffold -- --template svelte-ts --manager npm --yes
# move generated files into claude-monitor without clobbering docs/assets/.git/.gitignore
rsync -a --exclude .git --exclude .gitignore cm-scaffold/ claude-monitor/
rm -rf cm-scaffold
```
- [ ] **Step 2:** Convert to a workspace: create root `Cargo.toml` with `[workspace] members=["crates/core","src-tauri"]`, add `crates/core` (`cargo new --lib crates/core`), add `core = { path = "../crates/core" }` to `src-tauri/Cargo.toml`. Add `rust-toolchain.toml` pinning `stable`.
- [ ] **Step 3 (GATE):** Verify dev toolchain builds the empty app.
```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /c/Users/Thomas/Documents/Projects/claude-monitor
npm install
cargo build --manifest-path src-tauri/Cargo.toml 2>&1 | tail -20   # must succeed
```
Expected: `Finished`. If WebView2 missing at runtime, install via `winget install Microsoft.EdgeWebView2Runtime` (Win11 usually ships it).
- [ ] **Step 4: Commit** `chore: scaffold Tauri 2 + Svelte-TS workspace`.

> If Step 3 fails on link/MSVC, wrap cargo in vcvars: `cmd //c '"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" && set "PATH=%USERPROFILE%\.cargo\bin;%PATH%" && cargo build'`. (Trivial build worked without it, so likely unnecessary.)

---

## Phase 1 — `core`: pure logic (TDD). Parallelizable per-module.

Common: `export PATH="$HOME/.cargo/bin:$PATH"`; run tests with `cargo test -p core`.

### Task 1.1: `model.rs` — types
- [ ] Create `crates/core/src/model.rs` with the types from the contract above. Add `pub mod model;` etc. to `lib.rs`. Commit `feat(core): data model`.

### Task 1.2: `paths.rs` — locate Claude data + project naming
- [ ] **Test first** (`tests/paths_tests.rs`):
```rust
use core::paths::project_name_from_cwd;
#[test] fn basename_of_cwd_is_project() {
    assert_eq!(project_name_from_cwd(r"C:\Users\Thomas\Documents\8111Reader"), "8111Reader");
    assert_eq!(project_name_from_cwd("/c/Users/Thomas/Documents/Projects/CorePilot"), "CorePilot");
}
#[test] fn empty_cwd_falls_back() { assert_eq!(project_name_from_cwd(""), "unknown"); }
```
- [ ] Run → fail. Implement `claude_home()` (`dirs::home_dir()/.claude`), `projects_dir()`, `sessions_dir()`, `project_name_from_cwd(&str)->String` (split on `/` and `\\`, take last non-empty, else "unknown"). Run → pass. Commit.

### Task 1.3: `parser.rs` — jsonl line → LineKind  (HIGH VALUE, full TDD)
- [ ] **Test first** (`tests/parser_tests.rs`) — use real-shaped lines:
```rust
use core::parser::parse_line;
use core::model::LineKind;

const ASSISTANT: &str = r#"{"type":"assistant","cwd":"C:\\Users\\Thomas\\Documents\\Projects\\CorePilot","sessionId":"abc","requestId":"req_1","timestamp":"2026-06-05T10:00:00.000Z","message":{"model":"claude-opus-4-8","stop_reason":"tool_use","content":[{"type":"tool_use","id":"tu_9","name":"Bash"}],"usage":{"input_tokens":100,"output_tokens":20,"cache_creation_input_tokens":50,"cache_read_input_tokens":2000}}}"#;

#[test] fn parses_assistant_usage() {
    match parse_line(ASSISTANT) { LineKind::Assistant(e) => {
        assert_eq!(e.request_id, "req_1");
        assert_eq!(e.model, "claude-opus-4-8");
        assert_eq!(e.project, "CorePilot");
        assert_eq!(e.usage.cache_read, 2000);
        assert_eq!(e.usage.total(), 2170);
    }, other => panic!("expected Assistant, got {other:?}") }
}
#[test] fn detects_tool_use() { /* line with only tool_use content -> ToolUse{name:"Bash"} when no usage */ }
#[test] fn detects_tool_result() { /* {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_9"}]}} */ }
#[test] fn end_turn_detected() { /* stop_reason":"end_turn" with no tool_use */ }
#[test] fn skips_non_usage_lines() { assert_eq!(parse_line(r#"{"type":"summary"}"#), LineKind::Other); }
#[test] fn corrupt_line_is_other_not_panic() { assert_eq!(parse_line("{not json"), LineKind::Other); }
#[test] fn half_written_line_is_other() { assert_eq!(parse_line(r#"{"type":"assi"#), LineKind::Other); }
```
- [ ] Run → fail. Implement with serde_json `Value` (tolerant): parse to `Value`; if invalid → `Other`. Extract `message.usage` → `Assistant(ParsedEvent)` (request_id from top-level `requestId` else `uuid`; ts from `timestamp` ISO→millis; project from `cwd`; model from `message.model`). Else inspect `message.content[]`: a `tool_use` block → `ToolUse{id,name}`; a `tool_result` block → `ToolResult{tool_use_id}`. `message.stop_reason=="end_turn"` → `EndTurn`. Else `Other`. Run → pass. Commit `feat(core): tolerant jsonl parser`.

### Task 1.4: `pricing.rs` — cost estimate (full TDD)
- [ ] **Test first**: unknown model → `None`; known model computes `input*pin + output*pout + cache_create*pin*1.25 + cache_read*pin*0.10` (per million). Seed table editable.
```rust
use core::pricing::PriceTable; use core::model::Usage;
#[test] fn unknown_model_no_cost() { let t=PriceTable::seeded(); assert!(t.cost_usd(&Usage::default(),"made-up").is_none()); }
#[test] fn opus_cost_math() {
    let t=PriceTable::seeded();
    let u=Usage{input:1_000_000,output:0,cache_create:0,cache_read:0,..Default::default()};
    let c=t.cost_usd(&u,"claude-opus-4-8").unwrap();
    assert!((c - t.rate("claude-opus-4-8").unwrap().input).abs() < 1e-9);
}
```
- [ ] Implement `PriceTable { rates: HashMap<String,Rate>, fallback_by_prefix }` with `seeded()` (opus/sonnet/haiku 4.x prefixes; **TODO comment: confirm exact $/M from Anthropic pricing page** — values are config-overridable so correctness is data, not code). `cost_usd` returns `Option<f64>`. Load/save `pricing.json`. Run → pass. Commit.

### Task 1.5: `store.rs` — SQLite (schema, dedup insert, offsets) (full TDD)
- [ ] **Test first** (`tests/store_tests.rs`, use a temp file db):
```rust
use core::store::Store; use core::model::*;
fn ev(id:&str)->ParsedEvent{ ParsedEvent{request_id:id.into(),ts:1,session_id:"s".into(),project:"p".into(),model:"claude-opus-4-8".into(),usage:Usage{input:10,output:5,..Default::default()}} }
#[test] fn insert_is_idempotent_on_request_id() {
    let s=Store::open_in_memory().unwrap();
    s.insert_event(&ev("r1")).unwrap();
    s.insert_event(&ev("r1")).unwrap(); // dup
    assert_eq!(s.total_tokens().unwrap(), 15); // counted once
}
#[test] fn offsets_roundtrip() { let s=Store::open_in_memory().unwrap(); s.set_offset("f.jsonl",123).unwrap(); assert_eq!(s.get_offset("f.jsonl").unwrap(),123); }
#[test] fn store_path_not_under_dotclaude() { /* assert default db path contains "claude-monitor" and not ".claude\\projects" */ }
```
- [ ] Implement: `open(path)`/`open_in_memory()`, `PRAGMA journal_mode=WAL`, schema (`events(request_id PK, ts, session_id, project, model, input, output, cache_create, cache_read, web_search, web_fetch, source_file, line_offset)`, `import_state(file PK, byte_offset, schema_version)`), indexes on `ts,model,project`. `insert_event` uses `INSERT OR IGNORE`. Run → pass. Commit.

### Task 1.6: `query.rs` — aggregations → DTOs
- [ ] **Test first**: insert a few events across 2 models/2 projects/2 days; assert `summary("all")` totals, `byModel`, `byProject`, `timeseries` buckets, and `cache_hit_rate = cache_read/(input+cache_create+cache_read)`. Implement with SQL `GROUP BY`. Commit.

### Task 1.7: `state.rs` — PetState derivation (full TDD)
- [ ] **Test first** (`tests/state_tests.rs`): given a `sessions/<pid>.json` (status busy) + a synthetic jsonl tail, assert the derived `PetState`:
```rust
// busy + last meaningful line is ToolUse(Bash) with no matching ToolResult -> Working(Some("Bash"))
// busy + last is Thinking -> Thinking
// busy + last is Assistant text (no tool_use, stop_reason end_turn) -> Waiting
// status idle / heartbeat stale -> Idle ; very stale -> Sleeping
// unknown status -> Idle (fallback)
```
- [ ] Implement `derive_session_state(session_json, recent_lines:&[LineKind], now_ms) -> SessionState`. Pair tool_use/tool_result by id over the recent window. Thresholds as named consts (IDLE_MS=60_000, SLEEP_MS=600_000), debounced by caller. Commit.

### Task 1.8: `importer.rs` + `watcher.rs`
- [ ] `importer::backfill(projects_dir,&Store,progress_cb)`: walk `*.jsonl`, for each read from stored byte-offset, stream lines, `parse_line`, insert in a transaction (batch), update offset. **Integration test**: point at `tests/fixtures/projects/` (2 small jsonl) → assert totals; run twice → totals unchanged (idempotent).
- [ ] `watcher::watch(projects_dir, tx)`: `notify` recommended watcher, debounce 300ms, on modify read new bytes from offset, parse, send `ParsedEvent`/`LineKind` over an `mpsc`. **Integration test**: create temp dir, spawn watch, append a line, assert event received within 2s.
- [ ] Commit each.

### Checkpoint A — Codex review of `core`
- [ ] Run full `cargo test -p core` (all green) + `cargo clippy -p core -- -D warnings`.
- [ ] **Codex review** (use the `/codex` skill or `codex:rescue` agent) on `crates/core`: focus correctness of parser tolerance, dedup, cost math, state machine, and the read-only invariant. Apply fixes via superpowers:receiving-code-review (verify, don't blindly accept). Commit fixes.

---

## Phase 2 — `src-tauri`: server + shell

### Task 2.1: axum server (`src-tauri/src/server.rs`)
- [ ] Build `tokio::sync::broadcast` bus for SSE. Routes per contract. Static files via `tower-http::services::ServeDir` pointing at the built frontend (`../dist`) in release, or proxy to Vite dev server in debug. Bind `127.0.0.1:0`, write chosen port to app-data `server-port`. **Test**: a `#[tokio::test]` hitting `/api/summary` returns 200 + valid JSON shape against an in-memory store.
- [ ] Commit `feat(app): axum server + SSE bus`.

### Task 2.2: bootstrap (`main.rs`) — wire core into the app
- [ ] On setup: resolve app-data dir, open `Store`, run `backfill` in a background task (emit `import` SSE progress), start `watch` task (feed store + broadcast `usage`), start a `state` poll task (every ~1s diff `sessions/*.json` + jsonl tails → broadcast `sessions`). All read-only on `~/.claude`. Commit.

### Task 2.3: tray (`tray.rs`)
- [ ] Tray icon + tooltip/menu showing current-session tokens, cache %, today cost; menu: Open Dashboard, Quit. Updates from the `usage`/`current` data. Commit.

### Task 2.4: windows (`windows.rs`)
- [ ] Dashboard window → loads `http://127.0.0.1:<port>/`. Pet windows: subscribe to `sessions`; for each active session spawn a **transparent, always-on-top, decorations:false, skipTaskbar** window at `/pet?session=<id>`; despawn (after a short delay to allow leave-animation) when a session disappears. Persist per-session window positions to `pet-positions.json`; cascade new ones. Soft cap 8 → compact arrangement. Commit.

### Checkpoint B — Codex review of `src-tauri`
- [ ] `cargo build` clean; manual smoke (below). Codex review focus: task lifecycle, no blocking on the UI thread, proxy/localhost handling, window leak on session churn. Fix + commit.

---

## Phase 3 — Frontend (Svelte). Parallelizable: dashboard vs pet.

### Task 3.1: api client + formatting (`src/lib/api.ts`, `format.ts`)
- [ ] `api.ts`: `getSummary(range)`, `getCurrent()`, `getSessions()`, and `subscribe(onEvent)` (EventSource to `http://127.0.0.1:<port>/events`; port from a `<meta>` injected by the server or `/server-port`). Use `127.0.0.1`. **Vitest** unit tests for `format.ts` (tokens→"1.24M", cost→"$4.10", pct). Commit.

### Task 3.2: Dashboard (`Dashboard.svelte` + components)
- [ ] KPI row, ECharts stacked timeseries, model donut, project bars, cache panel, current-session strip, range tabs, live indicator (updates via SSE). Dark theme per spec §6.1. **Vitest + @testing-library/svelte**: each component renders with mock data; range switch refetches. Commit.

### Task 3.3: Clawd pet (`pet/clawd-grid.ts`, `Clawd.svelte`, `pet-main.ts`)
- [ ] `Clawd.svelte` renders the rig from `GRID` as grouped SVG `<g class="p-body|p-eyes|p-arml|p-armr|p-leg p-legN">` + per-state CSS animations (reuse the validated keyframes from the brainstorm `clawd-rig.html`: bob/tilt/jit/breathe/blink/step/pump/lookx/floatz). `pet-main.ts` reads `?session`, subscribes SSE `sessions`, maps `PetState`→state class, shows project label + tool tag + bubble; transparent body; drag to move (Tauri window drag); click → open dashboard. **Vitest**: state→class mapping table; grid→parts integrity (eye count=2, legs=4, arms split). Commit.

### Checkpoint C — Codex review of frontend
- [ ] `npm run build` clean; `npm run test` green. Codex review focus: SSE reconnect/backoff, no memory leaks on re-render, animation perf. Fix + commit.

---

## Phase 4 — Integration, hardening, verification

### Task 4.1: end-to-end smoke (real data, read-only)
- [ ] `cargo tauri dev` (or build). Verify: backfill progress completes; dashboard shows non-zero totals from real `~/.claude`; tray shows current session; opening a second Claude Code session spawns a 2nd pet labeled by project; closing it plays leave + despawns. Capture a screenshot.
- [ ] **Assert read-only**: before/after run, hash `~/.claude/projects` mtimes or use a filesystem audit; confirm zero writes under `.claude`.

### Task 4.2: robustness pass
- [ ] Inject a corrupt/half line into a temp fixture → app keeps running, counts "N unparsed". Kill the app mid-backfill → relaunch resumes from offsets, no double counting. Proxy set (`HTTP_PROXY`) → dashboard still loads (127.0.0.1 + NO_PROXY). Unknown model → cost shows "—". 8+ sessions → compact arrangement.

### Task 4.3: build the release artifact
- [ ] `npm run tauri build` → MSI/NSIS in `src-tauri/target/release/bundle/`. Record path + size. (If WebView2 runtime needed, note in README.)

### Task 4.4: README + run instructions
- [ ] `README.md`: what it does, how to run (`npm run tauri dev`), how to build, the `~/.cargo/bin` PATH note, the proxy/localhost note, the Clawd IP note, and the editable `pricing.json` location.

### Checkpoint D — final Codex review + verification-before-completion
- [ ] Full `cargo test`, `cargo clippy -D warnings`, `npm test`, `npm run build`, `cargo tauri build` all green. Final Codex pass over the whole diff. Use superpowers:verification-before-completion: paste real command output into the morning report; no "it works" without evidence.

---

## Parallel-agent execution map (what the user asked for)

- **Wave 1 (after Phase 0 gate):** dispatch up to 3 subagents on disjoint dirs:
  - Agent-Core → Phase 1 (`crates/core/**`) — owns all core modules + tests.
  - Agent-FE-Dash → Task 3.1–3.2 (`src/**` dashboard) against the pinned contract (mock server).
  - Agent-FE-Pet → Task 3.3 (`src/pet/**`) against the pinned `SessionState`/`PetState` contract.
  - (These three touch non-overlapping files → safe in parallel. Each runs its own tests before returning.)
- **Wave 2 (needs core):** I integrate Phase 2 (`src-tauri/**`) myself (depends on core API), then wire FE.
- **Codex review** at Checkpoints A–D via `/codex` or `codex:rescue`; fixes triaged with superpowers:receiving-code-review.
- **Debug agents:** on any red checkpoint, dispatch a focused subagent (or `codex:rescue`) with the failing command output to root-cause, using superpowers:systematic-debugging.
- Commit after every green task. Never mark a task done with failing tests (per user's testing rules).

## Self-review notes (done)

- **Spec coverage:** tray (§6.2)=2.3; dashboard (§6.1)=3.2; backfill+persist (§7)=1.5/1.8/2.2; cache metrics (§5.3)=1.6; cost (§5.4)=1.4; multi-pet per session + labels + leave anim (§12)=1.7/2.4/3.3; read-only (§2)=invariant tests; errors (§8/§12.5)=4.2. ✓
- **Type consistency:** `SessionState`/`PetState`/`Summary` defined once (contracts) and reused by Rust + TS mirror. SSE events named `usage|sessions|import` consistently. ✓
- **No placeholders** except the pricing $ values, which are intentionally data (config-overridable) with a TODO to confirm from the pricing page — does not block code.
