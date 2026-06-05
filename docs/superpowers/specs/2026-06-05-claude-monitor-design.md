# Claude Monitor — 实时 Token 用量监测器 · 设计文档

- **日期**：2026-06-05
- **状态**：设计已确认，待写实现计划（writing-plans）
- **作者**：Thomas（li.hairu@northeastern.edu）+ Claude

---

## 1. 概述

一个常驻后台的本地工具，**全面、可视化、持久地**监测我在 Claude Code 中的 token 用量。
读取 Claude Code 自己写的会话日志（`~/.claude/projects/**/*.jsonl`），抽取每条 assistant 消息的
token 用量（含缓存读/写明细），存入本地 SQLite，并通过两个展示层呈现：

- **系统托盘挂件**：随时瞟一眼当前会话的实时用量。
- **网页仪表盘**：图表化的多维拆解与历史趋势（Tauri 窗口，也可在 Chrome 打开 localhost）。

### 核心定位（与已有功能的区别）

Claude Code 自带 `~/.claude/usage-data/report.html`（"Claude Code Insights"）和 `/usage`，
但它们是**静态快照**。本工具的价值点在三处：**① 实时**（托盘 + SSE 推送）、
**② 持久累积**（自有 SQLite，重启/清缓存不丢，可回溯全部历史）、**③ 自定义全面可视化**。

---

## 2. 目标与非目标

### 目标
- 全面可见：按 **模型 / 项目 / 会话 / 日·时 / 缓存类型** 多维拆解 token。
- 实时：当前活跃会话的用量秒级更新。
- 持久：导入全部已有历史（~646MB / 551 文件 / 可回溯至约 2026-04），之后持续累积、永久保存。
- 缓存可见：突出缓存命中率与读/写/非缓存拆解。
- 成本估算：按 API 公开价估算 $（可编辑价格表，标注"估算"）。
- 轻量：常驻内存目标 30–60MB。

### 非目标（YAGNI）
- 不做云同步 / 多机聚合。
- 不写、不修改 Claude Code 的任何文件（严格只读）。
- 不做预聚合 rollup 表（明细 + 索引足够，留作后续性能优化）。
- 不做告警/限流功能（当前目的是"全面可见"，非"控成本/限额"；数据模型保留扩展空间）。

---

## 3. 关键决策（已与用户确认）

| 维度 | 决定 | 备注 |
|---|---|---|
| 形态 | **托盘挂件 + 网页仪表盘** 组合 | Tauri 天然统一两者 |
| 主要目的 | **全面可见性**（多维拆解） | 成本/限额作为维度纳入，非唯一焦点 |
| 数据范围 | **全部历史回填 + 持续累积 + 永久保存** | jsonl 是事实源，自有 SQLite 持久化 |
| 技术栈 | **Tauri（Rust 内核 + 网页前端）** | ~10MB、低内存；MIT/Apache 许可，符合远离 GPL 立场 |
| 成本 | **API 公开价估算 + 可编辑价格表**，标注"估算" | 订阅用户看到的是"等价 API 价值" |
| 前端 | Svelte + TS + Vite + ECharts | 实现细节，可在审阅时调整 |
| 项目位置 | `C:\Users\Thomas\Documents\Projects\claude-monitor` | 独立 git 仓库 |

---

## 4. 架构

```
📁 数据源 · ~/.claude/projects/**/*.jsonl   ← Claude Code 写，我们只读不改
        │
        ▼
🦀 Rust 核心（Tauri 后台，常驻）
   ① 回填导入器   首启扫全部 551 文件，流式解析入库，带进度
   ② 文件监听     notify 盯 projects 目录，新行追加即触发，识别活跃会话
   ③ JSONL 解析器  逐行抽 usage，按 request_id 去重，容错坏行
   ④ 聚合器       按 模型/项目/会话/日/缓存 汇总，估算 $ 成本
        │
        ▼
🗃️ SQLite 持久库 (rusqlite, WAL)
   events（每条消息明细，request_id 唯一） + import_state（文件偏移 + schema 版本）
        │
        ▼   API 层：内嵌 localhost HTTP + SSE
   ┌──────────────────┬───────────────────────────┐
   ▼                  ▼                            ▼
🔵 原生托盘挂件     🟦 Tauri webview 窗口        🟢（可选）Chrome 打开同一 localhost
  当前会话实时        完整仪表盘                    一套 web 应用，多入口
```

**设计原则**：核心只读 jsonl、算完落 SQLite；托盘与网页都是"瘦展示层"，读 SQLite + 接收 SSE 推送。
三层解耦、各自可独立测试。

### 组件清单（高内聚、低耦合）

| 单元 | 职责 | 接口 / 输入输出 | 依赖 |
|---|---|---|---|
| `parser` | 一行 jsonl → `ParsedEvent` 或 `None` | 纯函数，无副作用 | serde / serde_json |
| `store` | SQLite 读写：插入(去重)、查询聚合、偏移管理 | Rust API | rusqlite |
| `importer` | 回填编排：遍历目录、流式读、批量入库、记偏移、报进度 | 进度事件流 | parser, store |
| `watcher` | notify 监听、读新增字节、解析、插入、发实时事件 | 实时事件流 | parser, store, notify |
| `pricing` | 模型→单价、成本计算；可编辑价格表 | 纯函数 + 配置加载 | serde（读 config） |
| `query` | 由 store 产出仪表盘 DTO | Rust API → JSON | store |
| `server` | localhost HTTP + SSE，服务前端资源 + JSON API + 实时流 | HTTP/SSE | query, store, tokio/axum |
| `tray` | 原生托盘图标 + 小弹窗，订阅实时事件 | Tauri 系统托盘 | tauri, query |
| `app` | Tauri 主进程，装配上述全部 | — | 全部 |
| `frontend` | 仪表盘 UI：拉 API + 订阅 SSE + 渲染图表 | 浏览器 | Svelte, ECharts |

---

## 5. 数据模型与指标

### 5.1 每条 assistant 消息抽取的字段（来自 jsonl `message.usage`）

| 字段 | 含义 |
|---|---|
| `ts` | 时间戳（存 UTC） |
| `session_id` / `project` | 会话 ID / 项目（按目录归类） |
| `model` | 如 `claude-opus-4-8` |
| `input_tokens` | 非缓存输入 |
| `output_tokens` | 输出 |
| `cache_creation_input_tokens` | 写缓存（首次，1.25× input 价） |
| `cache_read_input_tokens` | 读缓存命中（0.1× input 价） |
| `web_search` / `web_fetch` | 服务端工具调用次数（`server_tool_use`） |
| `request_id`（兜底 `uuid`） | **去重键** |

> 注：用户消息、工具结果、summary、sidechain 等无 `usage` 的行直接跳过。

### 5.2 SQLite 表结构

- **`events`** — 每条消息一行。`UNIQUE(request_id)`（兜底 `uuid`），重复导入/重启不重复计数。
  额外列 `source_file`、`line_offset` 支持增量解析与排查。
- **`import_state`** — 每个 jsonl 已解析到的**字节偏移** + `schema_version`，实现"只读新增的行"。
- 仪表盘查询直接对 `events` 做 `GROUP BY`，在 `ts` / `model` / `project` 建索引。
- **不建 rollup 预聚合表**（YAGNI）：几十万行 `GROUP BY` 在 SQLite 上是毫秒级，够用数年；真慢了再加。
- SQLite 开 **WAL** 模式：watcher 写 + UI 读 并发安全。

### 5.3 核心指标

- **Token**：总量 / input / output / 写缓存 / 读缓存。
- **缓存命中率** = `cache_read ÷ (input + cache_creation + cache_read)`（输入侧有多少来自缓存）。公式可调。
- **缓存省下的 token** = `cache_read` 计数（这些以 0.1× 而非 1× 计价）；对应成本节省单独展示。
- **维度**：按 模型 / 项目 / 会话 / 日·时 / 随时间趋势。

### 5.4 成本（$）

- 内置**可编辑价格表**：各模型 input / output 单价，缓存写 = 1.25× input、缓存读 = 0.1× input。
- 种子价格按 Anthropic 官方公开价填入 opus-4.x / sonnet-4.x / haiku-4.x（**具体数值在实现时从官方定价页核对填入**）。
- 标注"估算（按 API 公开价）"；订阅用户理解为"等价 API 价值"。
- **未知模型**：价格表查不到 → 成本显示"—"并标记，token 照常计入。

---

## 6. UI 设计

### 6.1 网页仪表盘（单页概览，深色现代风）

- **顶栏**：标题 + 时间范围切换（今天 / 7天 / 30天 / 全部）+ live 实时指示。
- **4 个 KPI**：总 Token、估算成本、缓存命中率、会话/消息数。
- **主图 · Token 随时间**：按类型堆叠柱（非缓存输入 / 输出 / 读缓存 / 写缓存）。
- **按模型**：环形图（Opus / Sonnet / Haiku 占比）。
- **Top 项目**：横向条形排名（按 token）。
- **缓存效率**：命中率大数字 + 读/写/非缓存拆解。
- **当前会话**（底部条）：项目 · 模型 · 实时 token · 缓存率 · 成本 + 迷你火花线。
- 单页滚动；后续面板变多可改 Tab（概览/模型/项目/缓存/会话）——v1 先单页。

### 6.2 托盘挂件

- 托盘图标显示迷你数字/进度；悬停看摘要。
- 点击弹小窗：当前会话 token、缓存率、今日成本 + 迷你进度条 + "打开完整仪表盘"。

---

## 7. 实时机制与数据流

### 首次启动（回填）
扫描全部 jsonl → 流式逐行解析（不整文件读进内存）→ 事务批量入库、按 `request_id` 去重 →
在 `import_state` 记下每个文件字节偏移。前端显示进度条。

### 实时（常驻）
`notify` 监听 `~/.claude/projects` → 文件被追加 → 从上次偏移只读新增行 → 解析入库 →
更新聚合 → **SSE 推送**给前端 → 托盘 + 仪表盘即时刷新。文件事件 **250–500ms 防抖**。

### 活跃会话识别
最近被修改的 jsonl 即当前会话，托盘实时显示其累计。

### B+C 统一
Rust 核心内嵌 **localhost HTTP + SSE 服务**；Tauri 窗口 = 指向它的 webview；
也可在 Chrome 打开同一地址。一套 web 应用、多入口。

---

## 8. 错误处理与边界

| 场景 | 处理 |
|---|---|
| 写到一半的行 | 解析失败时**不推进偏移**，等下一个换行符再读（避免半行 JSON） |
| 真正损坏的行 | 跳过 + 记日志 + 计数；界面可显示"N 行无法解析"，不中断 |
| 无 usage 的行 | 干净跳过 |
| 重复导入 | `request_id` 唯一约束，不重复计数 |
| 文件被删/移动（worktree 清理） | watcher 处理 create/modify/delete，已入库数据保留 |
| 646MB 回填 | 流式 + 事务批量插入，内存平稳 |
| schema 漂移 | serde 容忍缺失/多余字段；存 `schema_version` |
| 并发 | 只读它们的 jsonl、绝不加锁；自有 SQLite 用 WAL |
| 未知模型定价 | 成本显示"—"并标记，token 照常计入 |
| App 没开的时段 | jsonl 是事实源，下次启动按偏移补齐，**不丢数据** |
| 时区 | 统一存 UTC，显示按本地时区 |

---

## 9. 测试策略（80% 覆盖 + TDD / AAA）

- **单元 (Rust)**：
  - `parser`：样本行 → usage；坏行 → 错误；无 usage → 跳过；缺字段容错。
  - `aggregator/query`：缓存命中率公式、按维度汇总。
  - `pricing`：成本计算；未知模型 → "—"。
  - `store`：去重（同 request_id 二次插入不增计数）；偏移读写。
- **集成**：
  - fixture jsonl 目录跑回填 → 库内总数符合预期。
  - 往临时 jsonl 追加 → 新事件出现（watcher）。
  - 重启 → 偏移续读、不重复计数。
- **前端**：仪表盘组件用 mock 数据渲染；时间范围筛选；SSE 模拟实时更新。
- **E2E（轻量）**：对 fixture 的 `~/.claude` 启动 → 托盘出数字、仪表盘加载。

---

## 10. 技术栈细节

- **后端**：Rust + Tauri 2.x；`notify`（文件监听）、`rusqlite`（SQLite，bundled + WAL）、
  `serde`/`serde_json`（解析）、`tokio` + `axum`（localhost HTTP + SSE）。
- **前端**：Svelte + TypeScript + Vite + ECharts。
- **托盘**：Tauri 系统托盘 API。
- **打包**：Tauri 出 Windows 安装包（msi/nsis）。

---

## 11. 待确认 / 假设（审阅时可改）

1. 项目名/位置：`claude-monitor` @ `Projects\claude-monitor`（独立 git 仓库）。
2. 前端框架 Svelte、图表库 ECharts（亦可换 React / uPlot 等）。
3. 价格表种子数值需在实现时从官方定价页核对填入。
4. 自动随 Windows 启动（开机自启）—— v1 暂不做，留待后续；先手动启动。
5. 是否需要导出（CSV/JSON）—— v1 暂不做（YAGNI），数据模型不阻碍后续添加。
