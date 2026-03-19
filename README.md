# LingClaw

<p align="center">
  <img src="static/branding/logo-wordmark.png" alt="LingClaw" width="420">
</p>

LingClaw 是一个用 Rust 构建的个人 AI 助手，围绕 **Skill + CLI + Loop** 三层架构设计。

- **Skill** — LLM 推理层：系统提示、模型路由、上下文裁剪、思维模式
- **CLI** — 工具执行层：安全的命令/文件/网络工具、沙盒路径、SSRF 防护、安装与更新
- **Loop** — 连接层：WebSocket 会话、多会话状态、流式输出、斜杠命令、持久化

整个后端约 10200 行 Rust（`src/main.rs` 以 6000 行为硬预算）。架构核心是一个 **ReAct 风格的受控状态机**——在保留结构化 tool calling 的前提下，引入 `Analyze → Act → Observe → Finish` 显式阶段，让每一轮决策可追踪、可审计。

## Features

- **9 标准工具**：`think`、`exec`、`read_file`、`write_file`、`patch_file`、`delete_file`、`list_dir`、`search_files`、`http_fetch`
- **2 主会话管理工具**：`list_sessions`、`delete_session`
- **16 斜杠命令**：`/new`、`/session_new`、`/switch`、`/rename`、`/model`、`/think`、`/react`、`/tool`、`/reasoning`、`/skills`、`/status`、`/usage`、`/clear`、`/help`、`/sessions`、`/delete`
- **双 Provider 模型路由**：OpenAI + Anthropic，支持 `provider/model` 和纯 model ID
- **Per-session 模型覆盖**：运行时通过 `/model` 切换
- **持久化多会话**：每个会话有独立工作区和磁盘存档
- **Bootstrap + Normal 双提示模式**：提示文件随会话创建、按模式动态加载
- **流式浏览器 UI**：Axum WebSocket 后端 + `static/` 前端，增量文本节点追加（`TextNode.nodeValue +=`）、统一 rAF 调度、智能跟随滚动、历史懒加载（初始渲染最近 50 条，工具调用链不切断）
- **`/new` 对话压缩**：将对话摘要追加到每日记忆，然后清空上下文
- **ReAct 显式状态机**：`match react_ctx.phase()` 驱动的 Analyze/Act/Observe/Finish 四阶段循环，`evaluate_finish()` 结构化完成判定，`auto_think_level()` 按循环深度动态调整推理预算
- **非破坏性 Observation 摘要**：大工具结果生成 WS 事件 + 系统提示注入，原始结果始终完整保留；错误工具标记 `[FAILED]` 并附带耗时
- **推理可见性控制**：默认开启 ReAct 阶段转换 WS 事件（`react_phase`），可通过 `/react on|off` 手动切换；浏览器前端会显示阶段切换，`done` 事件包含 `reason`（正常完成时 `complete` | `empty`，hard-cap 时 `hard_cap`）
- **结构化工具结果**：`ToolOutcome`（output + is_error + duration_ms），前缀式错误检测，必填参数预校验，`tool_result` WS 事件携带耗时和错误标记
- **原子持久化**：会话存档先写 `.tmp` 再 rename（Windows 兼容），加载时自动修剪不完整工具调用
- **会话版本控制**：`SESSION_VERSION = 4`，旧存档自动迁移并补齐 `show_tools` / `show_reasoning` / `show_react` 等字段默认值
- **上下文裁剪追踪**：Analyze 阶段裁剪后发送 `context_pruned` WS 事件，包含移除消息数
- **安全控制**：危险命令检测、沙盒路径解析、SSRF 阻断、重定向阻断、输出/文件大小上限

## Quick Start

```bash
bash scripts/install-linux.sh   # Linux 一键安装/构建/部署 static/ 并执行安装后自检

cargo build --release
cargo install --path .
mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin/static"
cp -R static/. "${CARGO_HOME:-$HOME/.cargo}/bin/static/"

# 首次运行打开设置向导
lingclaw

# 服务管理
lingclaw start
lingclaw stop
lingclaw restart
lingclaw status
lingclaw update
lingclaw install
lingclaw install -d /path/to/source
lingclaw systemd-install        # Linux: 安装并启用 lingclaw.service
lingclaw health
lingclaw help
lingclaw --version
```

手动执行 `cargo install --path .` 时，必须把 `static/` 一并复制到安装目录旁边，否则 `http://127.0.0.1:18989/` 会返回 404。优先推荐直接使用 `bash scripts/install-linux.sh`。

服务启动后访问 http://127.0.0.1:18989 。

也可以只用环境变量：

```bash
# OpenAI
OPENAI_API_KEY=sk-xxx lingclaw

# Anthropic
ANTHROPIC_API_KEY=sk-ant-xxx LINGCLAW_MODEL=claude-sonnet-4-20250514 lingclaw
```

## Configuration

配置文件在 `~/.lingclaw/.lingclaw.json`，首次运行由设置向导自动写入。

```json
{
  "settings": {
    "port": 18989,
    "execTimeout": 30,
    "maxContextTokens": 32000,
    "maxOutputBytes": 51200,
    "maxFileBytes": 204800
  },
  "models": {
    "providers": {
      "openai": {
        "baseUrl": "https://api.openai.com/v1",
        "apiKey": "sk-your-openai-key",
        "api": "openai-completions",
        "models": [
          {
            "id": "gpt-4o-mini",
            "name": "gpt-4o-mini",
            "reasoning": false,
            "input": ["text", "image"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 16384
          }
        ]
      },
      "anthropic": {
        "baseUrl": "https://api.anthropic.com",
        "apiKey": "sk-ant-your-anthropic-key",
        "api": "anthropic",
        "models": [
          {
            "id": "claude-sonnet-4-20250514",
            "name": "claude-sonnet-4-20250514",
            "reasoning": false,
            "input": ["text", "image"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 8192
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini"
      },
      "models": {
        "openai/gpt-4o-mini": {},
        "anthropic/claude-sonnet-4-20250514": {}
      }
    }
  }
}
```

说明：

- 推荐使用 `provider/model` 格式引用模型
- 多个 provider 暴露同一 model ID 时，必须使用显式前缀
- 遗留字段 `settings.provider`、`settings.apiKey`、`settings.apiBase` 仍被读取以保持向后兼容

## Environment Variables

| 变量 | 默认值 | 说明 |
|---|---|---|
| `OPENAI_API_KEY` | provider 配置或空 | OpenAI API Key |
| `ANTHROPIC_API_KEY` | provider 配置或 `OPENAI_API_KEY` | Anthropic API Key |
| `LINGCLAW_PROVIDER` | 自动检测 | 强制指定 `openai` 或 `anthropic` |
| `OPENAI_API_BASE` | `https://api.openai.com/v1` | 备用 API Base |
| `LINGCLAW_MODEL` | `gpt-4o-mini` | 默认模型 |
| `LINGCLAW_PORT` | `18989` | HTTP 端口 |
| `LINGCLAW_EXEC_TIMEOUT` | `30` | Shell 命令超时（秒） |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | `32000` | 默认上下文 token 预算 |

## Slash Commands

| 命令 | 说明 |
|---|---|
| `/new` | 压缩对话到每日记忆，清空上下文 |
| `/session_new` | 创建新会话 |
| `/switch <id>` | 切换到另一个会话 |
| `/rename <name>` | 重命名当前会话 |
| `/model [name]` | 查看可用模型或切换当前会话模型 |
| `/think [level]` | 设置思维模式：`auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh` |
| `/react [on\|off]` | 切换 ReAct 阶段可见性（默认开启；启用后每次阶段转换发送 `react_phase` WS 事件） |
| `/tool [on\|off]` | 切换工具卡片显示；该设置会持久化到当前 session 的视图状态 |
| `/reasoning [on\|off]` | 切换 reasoning 面板显示；该设置会持久化到当前 session 的视图状态 |
| `/skills` | 列出可用工具帮助 |
| `/status` | 显示模型、provider、上下文估算、最大输出 token、思维级别，token 数值按 K/M 显示 |
| `/usage` | 显示当前 session 的累计输入、输出、总 token 估算用量，以及今日输入、输出、总量估算；同时显示内存与磁盘上所有 session 合并后的今日总 token 估算，按 K/M 显示 |
| `/clear` | 清空消息但保留系统提示 |
| `/help` | 命令帮助 |
| `/sessions` | 仅主会话：列出活跃会话 |
| `/delete <id>` | 仅主会话：按完整 ID 或唯一前缀删除会话 |

## Tools

| 工具 | 说明 |
|---|---|
| `think` | 内部推理便签 |
| `exec` | 运行 shell 命令，带超时和危险命令过滤 |
| `read_file` | 读文件，支持可选行范围 |
| `write_file` | 创建或覆写文件 |
| `patch_file` | 查找替换文件片段 |
| `delete_file` | 删除文件 |
| `list_dir` | 列目录内容 |
| `search_files` | 正则搜索工作区文件 |
| `http_fetch` | HTTP GET，带 SSRF 防护和重定向阻断 |
| `list_sessions` | 仅主会话：查看会话状态 |
| `delete_session` | 仅主会话：删除会话 |

---

## Architecture

### 总体视图

```text
┌──────────────────────────────────────────────────────────────────┐
│                         Browser (static/)                        │
│   index.html  ·  app.js  ·  style.css                           │
└────────────────────────┬─────────────────────────────────────────┘
                         │ WebSocket /ws
┌────────────────────────▼─────────────────────────────────────────┐
│                     Axum HTTP Server                             │
│   GET /api/health · GET /api/sessions · POST /api/shutdown       │
│   GET /ws (WebSocket upgrade)                                    │
└────────────────────────┬─────────────────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────────────────┐
│                    Connection Layer (Loop)                        │
│   handle_socket() · handle_command() · session persistence       │
│   active_connections · session_clients · live_rounds             │
│   avatar polling · replay/rebind · cooperative shutdown          │
└───────┬──────────────────┬───────────────────┬───────────────────┘
        │                  │                   │
┌───────▼───────┐  ┌───────▼────────┐  ┌──────▼────────┐
│  Agent Loop   │  │  Session Store │  │  Config       │
│  ReAct FSM    │  │  多会话持久化    │  │  模型路由      │
│  ≤200 rounds  │  │  隔离工作区      │  │  环境变量回退   │
└───┬───────┬───┘  └────────────────┘  └───────────────┘
    │       │
┌───▼───┐ ┌─▼──────────────────┐
│ Skill │ │      CLI           │
│ Layer │ │    (Tools)         │
└───┬───┘ └───┬────────────────┘
    │         │
┌───▼─────────▼────────────────────────────────────────────────────┐
│                      Provider Layer                               │
│   call_llm_stream() → OpenAI SSE / Anthropic SSE                 │
│   ResolvedModel · thinking/reasoning 参数映射                      │
│   tool_definitions() · tool_definitions_anthropic()               │
└──────────────────────────────────────────────────────────────────┘
```

### 三层架构：Skill + CLI + Loop

| 层 | 职责 | 代码位置 |
|---|---|---|
| **Skill** | LLM 推理、系统提示构建、上下文裁剪、token 估算、思维模式 | `src/main.rs`（`build_system_prompt`, `prune_messages`, `estimate_tokens`）、`src/providers.rs`（流式调用）、`src/prompts.rs`（模板加载） |
| **CLI** | 工具注册/分发/执行、路径沙盒、危险命令检测、SSRF 防护 | `src/tools/mod.rs`（注册表）、`src/tools/fs.rs`（文件工具）、`src/tools/net.rs`（网络工具）、`src/tools/exec.rs`（执行工具） |
| **Loop** | WebSocket 处理、会话生命周期、斜杠命令、持久化、HTTP API | `src/main.rs`（`handle_socket`, `handle_command`, session 管理） |

### ReAct 状态机

Agent Loop 采用显式的 **ReAct 风格有限状态机**，将经典 ReAct 的 Thought → Action → Observation 循环转化为结构化阶段控制：

```text
         ┌──────────────────────────────────────────────┐
         │                Agent Loop                     │
         │         (max 200 rounds per turn)             │
         │                                               │
         │  ┌─────────┐    ┌─────────┐    ┌──────────┐  │
  user ──►  │ Analyze │───►│   Act   │───►│ Observe  │  │
  msg    │  └─────────┘    └─────────┘    └────┬─────┘  │
         │       ▲                              │        │
         │       └──────────────────────────────┘        │
         │                                               │
         │                ┌──────────┐                   │
         │                │  Finish  │──► response       │
         │                └──────────┘                   │
         └───────────────────────────────────────────────┘
```

| 阶段 | 含义 | 行为 |
|---|---|---|
| **Analyze** | 分析用户意图 | 模型分析请求，决定是直接回答还是使用工具。可借助 `think` 工具作为推理便签。 |
| **Act** | 执行工具 | 模型发出结构化 tool_calls，runtime 调用 `execute_tool()` 执行。所有路径经过安全检查。 |
| **Observe** | 消化工具结果 | 工具结果以原始内容写入对话历史。大结果 (>4KB) 生成非破坏性摘要：WS `observation` 事件 + 系统提示注入。 |
| **Finish** | 完成回答 | 显式判定任务已完成：请求已回答、修改已执行、验证已通过、无剩余 blocker。退出循环。 |

**关键设计决策：**

- **不回退到文本协议**：保留 OpenAI/Anthropic 原生结构化 tool calling，不使用文本版 `Action: tool_name\nAction Input: {...}` 解析
- **不污染对话历史**：完整思维链仅在 `think` 工具内部或 provider reasoning stream 中存在，不写入主消息序列
- **推理可见性已实现**：默认启用 `react_phase` WS 事件，前端会显示阶段切换；可通过 `/react off` 关闭；`done` 事件始终包含结构化 `reason` 字段
- **provider 层感知状态**：`auto` 模式下 `auto_think_level()` 根据循环深度动态调整推理预算（首轮 medium / 有 observation 时 high / 深轮 low）

### Agent Loop 详解

```text
handle_socket()
  │
  ├─ 收到用户消息
  │    ├─ 以 "/" 开头? → handle_command()
  │    └─ 否 → 进入 Agent Loop
  │
  ├─ 'agent: loop (round < 200, match react_ctx.phase())
  │    │
  │    ├─ AgentPhase::Analyze
  │    │    ├─ 构建 system prompt + 注入 observation hint
  │    │    ├─ auto_think_level() 计算有效推理级别
  │    │    ├─ prune messages
  │    │    ├─ call_llm_stream() → 流式输出到前端
  │    │    ├─ evaluate_finish() → Finish(reason) | Continue
  │    │    ├─ 有 tool_calls → transition_to_act()
  │    │    └─ 无 tool_calls → transition_to_finish(reason)
  │    │
  │    ├─ AgentPhase::Act
  │    │    ├─ 安全检查
  │    │    ├─ execute_tool() × N
  │    │    ├─ 收集 ToolResultEntry
  │    │    ├─ 持久化 tool result 到 session
  │    │    └─ transition_to_observe()
  │    │
  │    ├─ AgentPhase::Observe
  │    │    ├─ summarize_observations() → WS observation 事件
  │    │    ├─ build_observation_context_hint() → 下轮 hint
  │    │    ├─ 增量保存 session
  │    │    └─ transition_to_analyze()
  │    │
  │    └─ AgentPhase::Finish
  │         ├─ 增量保存 session
  │         ├─ WS done 事件
  │         └─ break
  │    │
  │    └─ cancel / timeout → 安全退出
  │
  └─ 返回控制权给 WebSocket 读循环
```

### 模块地图

```text
src/
├── main.rs          (~3560 行) — Config, Session, Agent Loop (phase-driven), 命令处理, HTTP 路由
├── main_tests.rs    (~2290 行) — 主流程测试 + observation 摘要 + replay/resume + 工具协议 + 持久化测试
├── agent.rs         (~410 行)  — AgentPhase 状态机, FinishReason, evaluate_finish, auto_think_level, Observation 摘要
├── cli.rs           (~1370 行) — CLI 子命令, 设置向导, PATH/systemd, 安装/更新
├── providers.rs     (~840 行)  — OpenAI/Anthropic 流式调用, 模型解析
├── prompts.rs       (~720 行)  — 提示文件初始化/加载, bootstrap baseline, 头像解析
└── tools/
    ├── mod.rs       (~450 行)  — ToolSpec 注册表, tool_definitions(), execute_tool(), ToolOutcome, 参数校验
    ├── fs.rs        (~310 行)  — read_file, write_file, patch_file, delete_file, list_dir, search_files
    ├── net.rs       (~180 行)  — http_fetch, check_ssrf, is_private_ip
    └── exec.rs      (~60 行)   — exec (shell), think (scratchpad)

static/
├── index.html                  — 主页面
├── app.js                      — 前端逻辑（流式渲染、懒加载历史、智能滚动）
└── style.css                   — 样式

docs/reference/templates/       — 7 个提示模板文件 (BOOTSTRAP/AGENTS/IDENTITY/SOUL/USER/TOOLS/MEMORY.md)
```

### 核心数据结构

```rust
enum Provider { OpenAI, Anthropic }

struct Config {
    api_key, api_base, model, provider,
    providers: HashMap<String, JsonProviderConfig>,
    port, max_context_tokens, exec_timeout,
    max_output_bytes, max_file_bytes,
}

struct Session {
    id, name, messages: Vec<ChatMessage>,
    created_at, updated_at, tool_calls_count,
    model_override: Option<String>,
    think_level: String,       // "auto"|"off"|"minimal"|"low"|"medium"|"high"|"xhigh"
    workspace: PathBuf,        // ~/.lingclaw/{id}/workspace/
    avatar: Option<String>,    // transient, 不序列化
    show_tools: bool,
    show_reasoning: bool,
    version: u32,              // 会话版本 (当前 SESSION_VERSION = 4)
}

struct ChatMessage {
    role: String,              // "system"|"user"|"assistant"|"tool"
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
    tool_call_id: Option<String>,
    timestamp: Option<u64>,
}

struct ResolvedModel {
    provider, api_base, api_key, model_id,
    reasoning: bool,
    thinking_format: Option<String>,  // "qwen"|"openai"|"anthropic"
    max_tokens: Option<u64>,
}

// Agent 状态机 (src/agent.rs)
enum AgentPhase {
    Analyze,    // 分析用户意图，构建推理计划
    Act,        // 执行工具调用
    Observe,    // 消化工具结果，更新理解
    Finish,     // 完成回答，退出循环
}
```

### Provider 层

双 Provider 支持，统一的调用接口：

```text
call_llm_stream(http, resolved, messages, tx, think_level, extra_tools)
    │
    ├─ resolved.provider == OpenAI
    │    └─ call_llm_stream_openai()
    │         ├─ convert_messages_to_openai()
    │         ├─ tool_definitions()
    │         ├─ think_level → reasoning_effort 映射
    │         └─ SSE 流解析 → WebSocket 转发
    │
    └─ resolved.provider == Anthropic
         └─ call_llm_stream_anthropic()
              ├─ convert_messages_to_anthropic()
              ├─ tool_definitions_anthropic()
              ├─ think_level → budget_tokens 映射
              └─ SSE 流解析 → WebSocket 转发
```

think_level 映射：

| level | OpenAI reasoning_effort | Anthropic budget_tokens |
|---|---|---|
| off | — | — |
| minimal | low | 1024 |
| low | low | 4096 |
| medium | medium | 10240 |
| high | high | 16384 |
| xhigh | high | 32768 |
| auto | model 支持 reasoning? medium : off | 同左 |

### 安全架构

```text
用户输入
  │
  ├─ Shell 命令 → check_dangerous_command() → 拒绝/放行
  │
  ├─ 文件路径 → resolve_path_checked(user_path, workspace)
  │               → 禁止逃逸工作区沙盒
  │
  ├─ HTTP URL → check_ssrf(url)
  │               → 仅允许 http/https 协议
  │               → DNS 解析后拒绝私有 IP
  │               → 禁用重定向 (redirect::Policy::none)
  │
  ├─ 输出大小 → max_output_bytes (50KB 默认)
  │
  └─ 文件大小 → max_file_bytes (200KB 默认)
```

关键安全规则：
- 所有工具执行经过 `execute_tool()` 统一分发
- `resolve_path_checked()` 用于用户提供的路径（逃逸即报错），`resolve_path()` 仅用于内部沙盒归一化
- 网络工具为每个请求创建独立 `Client`，不复用共享 HTTP 客户端
- Shell 命令有可配置超时（默认 30s）
- 生产路径禁止 `.unwrap()`

### 会话与持久化

```text
~/.lingclaw/
├── .lingclaw.json          — 全局配置
├── sessions/
│   ├── main.json           — 主会话存档
│   └── {uuid}.json         — 子会话存档
├── main/workspace/         — 主会话工作区
│   ├── AGENTS.md           — 核心代理行为
│   ├── IDENTITY.md         — 身份/头像
│   ├── SOUL.md             — 高层推理规则
│   ├── USER.md             — 用户特定行为
│   ├── TOOLS.md            — 工具使用指南
│   ├── MEMORY.md           — 持久记忆指南
│   └── memory/
│       └── 2026-03-17.md   — 每日记忆
└── {uuid}/workspace/       — 子会话工作区 (同结构)
```

提示加载模式：

| 模式 | 条件 | 加载文件 |
|---|---|---|
| **Bootstrap** | `BOOTSTRAP.md` 存在 | `BOOTSTRAP.md + AGENTS.md` |
| **Normal** | `BOOTSTRAP.md` 不存在 | `AGENTS.md + IDENTITY.md + USER.md + SOUL.md`，然后加载当前 session 的 `MEMORY.md` + 今日/昨日记忆 |

关键不变式：
- 当 `IDENTITY.md` 和 `USER.md` 中的关键字段已被有效填写后，后端会自动删除 `BOOTSTRAP.md` 并切换到 Normal 模式
- `/new` 只压缩对话 + 写入记忆 + 清空上下文，不重建 session
- 重连不重建 `BOOTSTRAP.md`
- 每轮 agent loop 后增量保存 session（原子写入：write .tmp → rename）
- 会话切换前先保存到磁盘，失败时保留内存副本供重连恢复
- 加载时自动修剪不完整的工具调用事务（`trim_incomplete_tool_calls`）

### WebSocket 协议

客户端 → 服务端：

```json
{"type": "chat", "content": "用户消息", "session": "main"}
```

服务端 → 客户端：

| type | 用途 |
|---|---|
| `session` | 首次连接时的当前会话信息 |
| `history` | 当前会话历史消息 |
| `view_state` | `show_tools` / `show_reasoning` / `show_react` 状态同步 |
| `avatar_update` | 会话头像更新 |
| `start` | 新一轮回复开始 |
| `delta` | 流式文本片段 |
| `thinking_start` | 思维模式开始 |
| `thinking_delta` | 思维流式片段 |
| `thinking_done` | 思维模式结束 |
| `tool_call` | 工具调用开始 |
| `tool_result` | 工具执行结果（含 `duration_ms`、`is_error`） |
| `done` | 响应完成 |
| `react_phase` | ReAct 阶段转换（默认启用，可通过 `/react off` 关闭） |
| `observation` | 非破坏性工具结果摘要 |
| `context_pruned` | 上下文裁剪通知（含 `removed_count`） |
| `progress` | 命令处理中（不清除忙碌状态） |
| `success` | 命令成功（成功样式） |
| `system` | 中性系统消息 |
| `error` | 错误消息 |
| `session_created` | 新会话已创建 |
| `session_switched` | 已切换到目标会话 |
| `session_deleted` | 会话已删除 |
| `session_renamed` | 会话已重命名 |
| `sessions_list` | 会话列表 |
| `session` / `session_switched` | 当前会话已初始化或切换 |

## HTTP API

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/health` | GET | 健康检查 |
| `/api/sessions` | GET | 列出已知会话 |
| `/api/shutdown` | POST | 认证的本地关停端点（CLI 使用） |
| `/ws` | GET | WebSocket 升级端点 |

## Session Workspace

每个会话拥有独立工作区 `~/.lingclaw/{sessionId}/workspace/`，包含以下提示文件：

| 文件 | 用途 |
|---|---|
| `BOOTSTRAP.md` | 新会话的初始引导指令 |
| `AGENTS.md` | 核心代理行为 |
| `IDENTITY.md` | 身份/人格/头像来源 |
| `SOUL.md` | 高层推理规则 |
| `USER.md` | 用户特定行为指导 |
| `TOOLS.md` | 工具使用指导 |
| `MEMORY.md` | 持久记忆指导 |

每个工作区还有 `memory/` 子目录，存放 `memory/YYYY-MM-DD.md` 每日日志。

## License

MIT
