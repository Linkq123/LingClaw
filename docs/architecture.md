# LingClaw 架构

[简体中文](architecture.md) · [English](architecture.en.md) · [返回 README](../README.md)

LingClaw 是单进程 Rust Runtime 加静态浏览器前端。设计目标不是隐藏 Agent 的执行过程，而是在可检查的状态机、工具边界和持久化模型中运行它。

## 总体结构

```mermaid
flowchart TB
    Browser["Browser workspace"] <-->|"HTTP / WebSocket"| Server["Axum server"]
    Server --> Sessions["Session and Group runtime"]
    Sessions --> Loop["Analyze → Act → Observe → Finish"]
    Loop --> Prompt["Prompt, Skills, Memory, Context"]
    Loop --> Providers["OpenAI / Anthropic / Gemini / Ollama"]
    Loop --> Tools["Built-ins / MCP / Sub-agents"]
    Sessions <--> Disk["Atomic local persistence"]
    Tools --> Images["Optional S3 image pipeline"]
```

三个职责层：

- **Skill**：提示构建、模型路由、上下文裁剪、思考控制、Skills 和记忆注入。
- **CLI / Tools**：文件、shell、网络、Todos、MCP、图片和安全检查。
- **Loop**：WebSocket Session runtime、ReAct、Slash Commands、持久化、live replay 和后台任务。

## ReAct Runtime

`src/runtime_loop.rs` 驱动显式状态机：

| 阶段 | Runtime 行为 |
|---|---|
| Analyze | 固定本轮配置与模型快照，构建提示和请求预算，让模型回答或产生 tool calls |
| Act | 校验参数和权限，执行顺序/并行工具、MCP、Sub-agent 或 Orchestration |
| Observe | 把完整 tool result 写入消息，并生成非破坏性摘要、WorkingState 和可选 Task Plan |
| Finish | 完成流式回复、持久化 Session、触发可选 Memory/Reflection 后台工作 |

每轮维护临时 `WorkingState`，记录意图、目标、证据、已完成步骤、阻塞和下一步。它用于帮助循环决定继续还是结束，不替代原始消息和工具结果。

### 运行边界

- Agent run 使用启动边界取得的不可变 `Config` 和有效 Session model 快照，配置热更新不会让进行中的 run 落入另一模型。
- HTTP 级 LLM 重试只处理瞬态连接、超时、429 和 5xx；Agent cycle 是更高一层的决策循环。
- `/stop` 和服务关闭会取消当前 run，并向正在执行的工具和 Sub-agent 传播；各层 hard cap 与超时在各自边界终止工作。浏览器断开只解除连接，不会停止仍在运行的 active run。
- Busy 时收到的普通用户文本作为 delayed intervention 排队，在下一次 Analyze 前注入，不强行截断当前 tool transaction。
- Plan Mode 使用独立 `PlanOnly` 边界，只暴露只读能力；批准后用持久化 plan ID 开始正常 run。

### Execution Stack

后端保持细粒度 live events，前端按一次顶层 run 聚合 Reasoning、Tool、Task Plan、Sub-agent 和 Orchestration。跨多个 ReAct cycle 的步骤仍属于同一执行栈。Tool result 通过 tool-call ID 更新原步骤，而不是创建重复卡片。

历史记录没有可靠开始时间时，前端不伪造耗时。类型过滤后无可见步骤的执行栈会隐藏，并关闭相关 Inspector/Modal。

## Backend 模块职责

| 模块 | 主要职责 |
|---|---|
| `main.rs` | Axum 路由、HTTP/WS 安全、共享状态、配置事务、live replay |
| `runtime_loop.rs` | 顶层 Agent Analyze/Act/Observe/Finish |
| `agent.rs` | phase、TaskIntent、WorkingState、Task Plan、Finish 判定 |
| `providers.rs` | Provider 消息转换、请求、流解析和 usage |
| `config.rs` | JSON/环境变量加载、校验、模型解析和显式模型状态 |
| `commands.rs` | Slash Command |
| `context.rs` | Token 估算、请求预算、裁剪 |
| `hooks.rs` | LLM/Tool/Command 生命周期与自动上下文压缩 |
| `prompts.rs` | Workspace 提示、Bootstrap、Skills 发现与注入 |
| `session_store.rs` | Session schema、迁移和原子磁盘 I/O |
| `session_group.rs` | Group store、成员、管理员、投票和 replay payload |
| `session_control.rs` | Main-only 跨 Session/Group 控制平面和派发 |
| `todos.rs` | Todo 校验、revision 冲突和广播 |
| `memory.rs` | Structured Memory、Daily Reflection 和队列 |
| `image_uploads.rs` | PNG/JPEG 校验、S3 上传、签名和配置身份 |
| `tools/` | ToolSpec、执行分派、文件/shell/网络/MCP/view_image |
| `subagents/` | 发现、隔离执行和 DAG Orchestration |

`src/main.rs` 负责协议边界，不承载所有业务实现。模块测试位于 `src/tests/`，从对应源文件的测试模块引入。

## Provider 适配

Runtime 内部使用统一的 `ChatMessage`、tool call 和 `ToolOutcome`，`providers.rs` 转换为上游协议：

```mermaid
flowchart LR
    Internal["Internal messages + tools"] --> OpenAI["Chat Completions"]
    Internal --> Responses["OpenAI Responses"]
    Internal --> Anthropic["Anthropic Messages"]
    Internal --> Gemini["Gemini contents"]
    Internal --> Ollama["Ollama chat"]
    OpenAI & Responses & Anthropic & Gemini & Ollama --> Stream["Normalized live events"]
```

- OpenAI Chat 使用 SSE delta 和 `tool_calls`。
- OpenAI Responses 使用 `stream: true`，将 output text、reasoning summary 和 function call 事件映射到内部流。
- Anthropic 把连续 tool result 合并为用户内容块，并支持 prompt caching 与 thinking budget。
- Gemini 保留 `functionCall.id`、`functionResponse.id` 和真实 `thoughtSignature`，图片使用 `inlineData`。
- Ollama 消费 NDJSON stream，按模型能力发送 `think` 和 images。

Provider 的 reasoning effort 由统一 think level 和可选 `compat.thinkingFormat` 映射。辅助 Memory/Reflection/Context 调用进入相同 usage 记账，但不重放工具图片。

## 工具系统

`ToolSpec` 描述名称、说明、JSON schema 和执行属性。每次调用依次经过：

1. 工具是否在当前 run mode 和 Session policy 中可用。
2. 参数是否为对象、required/type/range/length 是否满足。
3. Hook 是否允许执行。
4. 工具自身 sandbox、超时和大小限制。
5. 结构化 `ToolOutcome` 记录 output、error、duration 和内存图片。

只读并行工具共享批次排序和图片预算。单个结果失败不破坏其他已完成结果；模型按原 tool-call 顺序收到 observation。

### MCP

MCP client 支持 stdio 和 Streamable HTTP：

- initialize、tools/resources/prompts 分页与 catalog 缓存
- ping、可选 roots、list-changed notifications
- Streamable HTTP POST/GET SSE
- OAuth PKCE、refresh token 和本地 token store
- 启动失败冷却、空闲 session 回收、超时取消
- Session server/tool policy 与 mutating tool 确认

MCP 暴露名带稳定 server/tool 标识，避免跨 server 冲突。resources/prompts 由用户浏览并手动插入，不自动变成模型工具。

### Sub-agent

Sub-agent executor 创建独立消息历史、过滤工具集和 mini-ReAct loop。父级只接收进度和最终文本结果。`task`、`orchestrate` 和共享 `todos` 被排除，避免无限递归或竞争同一状态。

Orchestrator 验证 DAG，按拓扑层并行运行，传播依赖结果并发送任务级事件。失败依赖会使后继任务失败或跳过，而无依赖任务可以继续。

## Session、Group 与持久化

```text
~/.lingclaw/
├── .lingclaw.json
├── mcp-auth.json
├── sessions/<session-id>.json
├── groups/<group-id>.json
├── system-skills/
├── system-agents/
├── skills/
├── agents/
└── <session-id>/workspace/
    ├── BOOTSTRAP.md
    ├── AGENTS.md
    ├── IDENTITY.md
    ├── SOUL.md
    ├── USER.md
    ├── MEMORY.md
    ├── structured_memory.json
    ├── memory/
    ├── skills/
    └── agents/
```

Session 存档包含消息、模型覆盖、think/view state、Todos、Skills policy、pending plan 和 usage。当前 `SESSION_VERSION = 7`；加载旧存档时填充默认字段并修剪不完整 tool transaction。

磁盘写入先创建 `.tmp` 再 rename，并针对 Windows 目标替换语义处理失败恢复。内存中已更新但落盘失败的 Session 不会立即丢弃，后续保存可以重试。

### Bootstrap prompt

- `BOOTSTRAP.md` 存在时加载 Bootstrap + AGENTS。
- 用户有效填写 IDENTITY/USER 后删除 Bootstrap，进入 Normal 模式。
- Normal 加载 AGENTS、IDENTITY、USER、SOUL、MEMORY 和今日/昨日记忆。
- 模板更新只影响新 Session，不覆盖已有工作区。
- YAML frontmatter 作为模板元数据保留，注入前剥离。

### Group 不变式

- Main 是隐式永久 Owner，不在 `members` 中作为普通派发对象。
- Promoted admins 存于 `admins[]`；管理员移除成员按 promoted-admin 票数计算 2/3，Owner 操作直接生效。
- 只有 `@session-id` 参与协议路由；显示名称不参与解析。
- queued/running member run 存在时禁止删除 Group，应先 stop。
- 失败或停止的 member run 不生成普通 Session 回复气泡，也不继续 mention follow-up。

## Live connection 与排序

浏览器主要通过 `/ws?session=<id>` 或 `/ws?group=<id>&session=main` 连接。初始化通常按 Session/Group metadata、view/model state、Todos、history 顺序回放。

活动 run 期间刷新页面时，新连接可以附着到 `live_round`，继续接收后续事件和终态，而不是重新执行已完成步骤。模型配置事件携带 `configRevision`；前端拒绝同一后端进程中的旧 revision，避免 Settings 保存、Session `/model` 与重连 payload 乱序。

Todos 使用独立 `todos_state` 和 `/api/todos` revision。配置文件保存使用独立 `configFileEtag` 做乐观并发；它与模型 `configRevision` 表达不同的排序域。

完整事件和请求结构见[后端接口文档](backend-api.md)。

## 图片数据流

```mermaid
sequenceDiagram
    participant T as Tool or MCP
    participant R as Runtime
    participant S as S3-compatible storage
    participant P as Vision model
    T->>R: Structured PNG/JPEG bytes
    R->>R: Validate magic, size, count, workspace
    R->>S: Upload with bounded concurrency
    S-->>R: Object key
    R->>R: Persist key + S3 identity + MIME
    R->>P: Fresh signed URL or local inline data
```

Runtime 不从任意文本、stdout、路径或 URL 猜测图片。原始 Base64 不进入日志、WebSocket、模型文本或 Session JSON。一个工具批次最多保留 10 张图片，上传并发最多 3，结果顺序与 tool calls 一致。

签名依赖 S3 配置身份；身份变化时旧 key 被跳过而不是用新配置重新签名。图片失败只追加“不可以使用”的文字说明，不改变原工具成功/失败状态。

## 前端架构

前端是 Vite + TypeScript，绝大多数工作台使用直接 DOM 渲染；Settings 和 Usage 是懒加载 React islands。构建输出写入 `static/`，Rust 直接提供静态文件。

主要职责：

- `main.ts`：入口和 live event switchboard
- `socket.ts`：连接、重连和 Session/Group 绑定
- `input.ts`：Composer、Slash、mention、图片和 send/stop
- `state.ts`：集中 UI state 与 DOM refs
- `renderers/execution-stack.ts`：顶层过程聚合
- `renderers/tools.ts`：Inspector 和图片画廊
- `actionDialog.ts`：Session/Group mutation 对话框
- `composerAvailability.ts`：显式模型配置门禁
- `pages/SettingsPage.tsx` / `UsagePage.tsx`：React pages

Markdown 经 marked、DOMPurify、highlight.js 和 KaTeX 处理。重复 decoration 必须幂等；代码块工具栏、mention 高亮和图片画廊不会在流式重渲染时重复生成。

## 安全边界

| 边界 | 约束 |
|---|---|
| Web | 只绑定 loopback；shutdown 使用本地 token |
| Files | `resolve_path_checked` 阻止逃逸 Session workspace；处理符号链接 |
| Shell | 危险命令规则、可配置超时、输出上限 |
| Network | 仅 HTTP/HTTPS，DNS 后拒绝私有目标，禁止 redirect |
| MCP | Session policy、workspace cwd、OAuth 本地存储、mutating 确认 |
| Images | PNG/JPEG 魔数、10MB、每批 10 张、S3 identity |
| Config | schema 校验、原子保存、ETag 和运行时快照 |

这些边界降低误操作和越权风险，但不构成虚拟机级隔离。Agent 获得 `exec` 或写工具后仍能在授予的工作区中修改数据；部署者需要根据模型和任务配置最小权限。
