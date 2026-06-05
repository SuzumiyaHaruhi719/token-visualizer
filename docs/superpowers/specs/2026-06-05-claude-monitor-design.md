# Claude Monitor — 实时 Token 用量监测器 · 设计文档

- **日期**：2026-06-05
- **状态**：设计已确认，待写实现计划（writing-plans）
- **作者**：Thomas（li.hairu@northeastern.edu）+ Claude

---

## 1. 概述

一个常驻后台的本地工具，**全面、可视化、持久地**监测我在 Claude Code 中的 token 用量。
读取 Claude Code 自己写的会话日志（`~/.claude/projects/**/*.jsonl`），抽取每条 assistant 消息的
token 用量（含缓存读/写明细），存入本地 SQLite，并通过三个展示层呈现：

- **系统托盘挂件**：随时瞟一眼当前会话的实时用量。
- **网页仪表盘**：图表化的多维拆解与历史趋势（Tauri 窗口，也可在 Chrome 打开 localhost）。
- **Clawd 桌宠**：官方吉祥物 Clawd 的像素桌宠，用**逐状态动画**实时反映 Claude 的工作状态（详见 §12）。

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
- 趣味/状态感知：Clawd 桌宠用**逐状态动画**实时反映 Claude 工作状态（思考/跑工具/回应/空闲等）。

### 非目标（YAGNI）
- 不做云同步 / 多机聚合。
- 不写、不修改 Claude Code 的任何文件（严格只读）。
- 不做预聚合 rollup 表（明细 + 索引足够，留作后续性能优化）。
- 不做告警/限流功能（当前目的是"全面可见"，非"控成本/限额"；数据模型保留扩展空间）。
- 桌宠 v1 不做**手绘逐帧美术**（用分层骨骼动画实现动态，见 §12.3）；专属手绘帧留作后续增强。

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
| 吉祥物 | **官方 Clawd**（橙色 8-bit 像素螃蟹）像素风 | 以用户提供的官方截图为基准精灵 |
| 桌宠呈现 | **自由桌宠**：透明、always-on-top、可拖动的独立窗口 | 浮在桌面任意位置 |
| 状态来源 | `~/.claude/sessions/<pid>.json` 的 `status` + 活跃 jsonl 尾部事件 | 粗粒度忙/闲 + 细粒度动作 |
| 状态表现 | **逐状态动画**（动态），跑工具时显示工具名 | v1 分层骨骼动画；手绘帧后续 |

---

## 4. 架构

```
📁 数据源 · ~/.claude/projects/**/*.jsonl（token+事件） + ~/.claude/sessions/<pid>.json（忙/闲状态）  ← 只读不改
        │
        ▼
🦀 Rust 核心（Tauri 后台，常驻）
   ① 回填导入器   首启扫全部 551 文件，流式解析入库，带进度
   ② 文件监听     notify 盯 projects 目录，新行追加即触发，识别活跃会话
   ③ JSONL 解析器  逐行抽 usage，按 request_id 去重，容错坏行
   ④ 聚合器       按 模型/项目/会话/日/缓存 汇总，估算 $ 成本
   ⑤ 状态推导器   读 sessions/<pid>.json status + jsonl 尾部事件 → 推导 PetState（含工具名）
        │
        ▼
🗃️ SQLite 持久库 (rusqlite, WAL)
   events（每条消息明细，request_id 唯一） + import_state（文件偏移 + schema 版本）
        │
        ▼   API 层：内嵌 localhost HTTP + SSE
   ┌────────────┬──────────────┬──────────────────┬─────────────┐
   ▼            ▼              ▼                  ▼
🔵 托盘挂件    🟦 webview 仪表盘  🟣 Clawd 桌宠       🟢（可选）Chrome
  当前会话实时    完整仪表盘        逐状态动画·透明置顶    打开同一 localhost
```

桌宠（🟣）也是订阅核心实时流的瘦展示层，只是订阅的是 `PetState` 而非 token 数字。

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
| `state` | 读 sessions/<pid>.json + 活跃 jsonl 尾部 → 推导 `PetState`（含工具名） | 纯函数 + 状态事件流 | parser, store |
| `tray` | 原生托盘图标 + 小弹窗，订阅实时事件 | Tauri 系统托盘 | tauri, query |
| `pet` | Clawd 桌宠：透明置顶窗，订阅 `PetState`，渲染逐状态动画 | 透明 Tauri 窗口 | tauri, state, frontend |
| `app` | Tauri 主进程，装配上述全部 | — | 全部 |
| `frontend` | 仪表盘 UI + 桌宠 UI：拉 API + 订阅 SSE + 渲染图表/动画 | 浏览器/webview | Svelte, ECharts |

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

> 注 1：用户消息、工具结果、summary、sidechain 等无 `usage` 的行直接跳过。
>
> 注 2：`project` 取自该行的 `cwd` 字段（取末段目录名，得到 CorePilot / 8111Reader 等友好名）；
> `cwd` 缺失时回退用 jsonl 所在目录名（`C--Users-...` 编码路径）解码。

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
多个会话并行时，托盘显示最近活跃的那个，仪表盘则聚合全部。

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
- **托盘 / 窗口**：Tauri 系统托盘 API；多窗口——仪表盘窗口 + **透明无边框 always-on-top 桌宠窗口**。
- **桌宠资产**：基准 Clawd 图预处理为带 alpha 的 PNG / 分层（或 SVG 矢量分层）以做骨骼动画。
- **打包**：Tauri 出 Windows 安装包（msi/nsis）。

---

## 11. 待确认 / 假设（审阅时可改）

1. 项目名/位置：`claude-monitor` @ `Projects\claude-monitor`（独立 git 仓库）。
2. 前端框架 Svelte、图表库 ECharts（亦可换 React / uPlot 等）。
3. 价格表种子数值需在实现时从官方定价页核对填入。
4. 自动随 Windows 启动（开机自启）—— v1 暂不做，留待后续；先手动启动。
5. 是否需要导出（CSV/JSON）—— v1 暂不做（YAGNI），数据模型不阻碍后续添加。
6. Clawd 是 Anthropic 的吉祥物/IP：用于**个人、不公开**的工具没问题；若日后公开分发需获 Anthropic 许可。
7. 桌宠精灵需把基准图的米白背景**去成透明**（chroma-key / 预处理出带 alpha 的 PNG）。
8. `sessions/<pid>.json` 的 `status` 取值枚举（已知 `busy`）需在实现时观察补全（如 `idle` 等）；状态映射对未知值兜底。
9. 桌宠 v1 用**分层骨骼动画**实现动态；专属手绘逐帧美术留作后续增强。

---

## 12. Clawd 桌宠（吉祥物状态显示）

把官方吉祥物 **Clawd**（橙色 8-bit 像素螃蟹）做成自由桌宠，用**逐状态动画**实时反映 Claude 的工作状态。
它是 token 监测之外的第三个展示层，**共用同一个 watcher**，不新增数据管道。

### 12.1 状态来源（两级）

- **粗粒度（忙/闲）**：`~/.claude/sessions/<pid>.json` 的 `status` 字段（已知 `busy`）+ `updatedAt` 心跳。
  心跳过期且进程不在 → 视为该会话已退出。
- **细粒度（具体动作）**：读「当前活跃会话」jsonl 的尾部事件类型：
  - 最近是 `thinking` 块、turn 未结束 → **思考中**
  - 有 `tool_use` 尚无配对 `tool_result` → **跑工具**（从该 `tool_use` 块的 `name` 取工具名）
  - 正在产出 `text` → **回应中**
  - 末条 assistant 消息 `stop_reason = end_turn` → **等你输入**
  - `status=idle` / 无新事件超过阈值 → **空闲**；更久 → **睡着了**
- **活跃会话**沿用 §7 的「最近活跃」逻辑；多会话并行时桌宠跟随最近活跃的那个。

### 12.2 PetState 状态机

| 状态 | 触发 | 动画表现 | 叠加 |
|---|---|---|---|
| 😌 空闲 idle | status=idle / 无活跃会话 | 缓慢上下浮、偶尔眨眼 | — |
| 🤔 思考中 thinking | busy + 最近 thinking 块 | 歪头晃动、身体轻微挤压 | 💭 想泡泡 |
| 🔧 跑工具 working | tool_use 未配对 tool_result | 腿部快速踱步、身体抖动 | 工具名标签（Bash/Edit/Read…） |
| 💬 回应中 responding | 正在产出 text | 上下跳动、眼睛灵动 | … 省略号 |
| ⏳ 等你输入 waiting | end_turn、会话还在 | 凑近呼吸、看着你 | 👀 |
| 💤 睡着了 sleeping | 长时间无活动 | 变暗、慢呼吸、闭眼 | 飘 z |

> 未知 `status` 值或无法判定 → 兜底为「空闲」。状态切换做去抖 / 最短停留，避免频繁跳变。

### 12.3 动画实现（动态）

用户要求**每个状态都动起来**。v1 不靠手绘 6 套帧，而是把基准 Clawd 精灵**分层骨骼化**：
身体 / 双眼 / 左右手 / 四条腿 拆成独立图层，用 CSS / Web 动画对各图层做关键帧
（眨眼、踱步、挤压拉伸、歪头），即可得到**真实的逐状态动态**，且只需一张源图。

- 资产：基准图预处理为带 alpha 的 PNG，并切出图层（或用 SVG 重绘为矢量分层，更利于变形）。
- 桌宠 UI 复用 `frontend` 技术栈（Svelte），在透明 Tauri 窗口里渲染。
- 后续增强：可替换 / 叠加**手绘逐帧**精灵以获得更强个性（非 v1 必需）。

### 12.4 桌面行为（自由桌宠）

- 独立的**透明、always-on-top、无边框**的 Tauri 窗口；位置可拖动并记忆。
- **悬停** → 气泡显示当前动作 + token 摘要（如「▶ 正在跑 Bash · 1.24M tok」）。
- **点击** → 打开完整仪表盘窗口。
- **右键** → 菜单：隐藏桌宠 / 切换置顶 / 退出。
- 多显示器：记住所在屏与坐标。

### 12.5 错误处理与边界（桌宠相关）

| 场景 | 处理 |
|---|---|
| `sessions/<pid>.json` 缺失/读不到 | 退化为纯 jsonl 推导；再不行 → 空闲 |
| 心跳 `updatedAt` 陈旧 | 判定会话已退出 → 空闲 / 睡着 |
| 多个 busy 会话 | 跟随最近活跃；右键菜单可切换跟随对象（后续） |
| 工具名缺失 / 未知 | 显示通用「跑工具」不带具体名 |
| 状态抖动 | 去抖 + 最短停留时间，避免频繁跳变 |
| 精灵背景残留 | 预处理出带 alpha 的资产，确保桌面透明 |

### 12.6 测试（桌宠相关）

- **单元**：`state` 推导器——给定 sessions.json + jsonl 尾部样本 → 期望 PetState；
  tool_use 未配对 → working + 正确工具名；end_turn → waiting；心跳过期 → idle；未知 status → 兜底。
- **集成**：构造 fixture（busy 的 sessions.json + 追加 tool_use 行）→ 桌宠收到 working 事件。
- **前端**：每个 PetState 渲染对应动画图层组合；状态切换去抖。
