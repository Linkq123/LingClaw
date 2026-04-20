# LingClaw

<p align="center">
  <img src="static/branding/logo-wordmark.png" alt="LingClaw" width="420">
</p>

LingClaw 是一个用 Rust 构建的个人 AI 助手，围绕 **Skill + CLI + Loop** 三层架构设计。

- **Skill** — LLM 推理层：系统提示、模型路由、上下文裁剪、思维模式、结构化记忆注入
- **CLI** — 工具执行层：安全的命令/文件/网络工具、沙盒路径、SSRF 防护、安装与更新
- **Loop** — 连接层：WebSocket 主会话、流式输出、斜杠命令、持久化、异步记忆更新

整个后端约 19900 行 Rust（`src/main.rs` 以 6000 行为硬预算）。架构核心是一个 **ReAct 风格的受控状态机**——在保留结构化 tool calling 的前提下，引入 `Analyze → Act → Observe → Finish` 显式阶段，让每一轮决策可追踪、可审计。

## Features

- **9 标准工具**：`think`、`exec`、`read_file`、`write_file`、`patch_file`、`delete_file`、`list_dir`、`search_files`、`http_fetch`；另有 2 个动态工具：`task`（子代理委托，发现代理时注册）、`orchestrate`（多代理 DAG 编排，发现代理时注册）
- **MCP servers（实验性）**：支持通过 `mcpServers` 配置接入 stdio 型 MCP server，使用当前 MCP JSON-RPC 传输约定，并将其 tools 以 `mcp__...` 名称前缀注入到模型工具列表；主 Agent 与子代理都会按需发现并使用这些 MCP tools；运行时会处理 `ping` / `roots/list` 请求，并在收到 `notifications/tools/list_changed` 后失效对应工具缓存；`start` / `restart` 会先做受限的一次性 preflight，`mcp-check` 可用于更深的运行时诊断；server 启动连续失败会进入短暂冷却，避免请求风暴
- **单主会话**：运行时固定使用 `main`，不再创建、切换或删除其他会话
- **子代理（Sub-Agents）**：支持通过 `task` 工具委托任务给专用代理（explore、researcher、coder、reviewer）；三层发现（system / global / session）、独立 ReAct 循环、Hook 集成、工具权限过滤（含 MCP 工具）
- **文档化斜杠命令**：`/new`、`/model`、`/think`、`/react`、`/tool`、`/reasoning`、`/stop`、`/skills`、`/skills-system`、`/skills-global`、`/skills-session`、`/agents`、`/status`、`/system-prompt`、`/mcp`、`/usage`、`/clear`、`/memory`、`/reflection`、`/help`
- **三 Provider 模型路由**：OpenAI + Anthropic + Ollama，支持 `provider/model` 和纯 model ID
- **主会话模型覆盖**：运行时通过 `/model` 切换 `main` 使用的模型
- **持久化主会话**：固定保存 `main` 工作区和磁盘存档
- **Bootstrap + Normal 双提示模式**：提示文件随会话创建、按模式动态加载
- **流式浏览器 UI**：Axum WebSocket 后端 + `static/` 前端，增量文本节点追加（`TextNode.nodeValue +=`）、统一 rAF 调度、智能跟随滚动、历史懒加载（初始渲染最近 50 条，工具调用链不切断）、版本号 badge（header + 欢迎页，从 `/api/health` 获取）、输入框上下键历史导航（最多 10 条）；Settings 页面支持在线编辑配置、Provider 连接测试、MCP Server 连接测试；Usage 页面显示 Token 用量统计、按 Model Role 拆分的明细卡片与图表
- **图片附件**：支持通过 URL 或本地 JPEG/PNG 上传附加图片到用户消息；本地上传需要配置顶层 `s3`（S3-compatible）并会把文件写入临时对象存储。OpenAI/Anthropic 直接消费现签 URL，因此对应 S3 端点必须能被远端 provider 访问；私网、localhost 或仅局域网可达的网关仅保证 Ollama 可用，因为 LingClaw 会本地预取为 base64 并持久化缓存到会话工作区；每条消息最多 10 张图片，支持 SSRF 防护、结构校验、10MB 大小上限；Agent 忙碌时发送的图片附件会被丢弃（仅保留文本干预）
- **运行中干预与中断**：Agent 忙碌时，输入框中的普通文本会作为“延迟干预”排队，在当前 ReAct 周期结束后、下一次 Analyze 前注入为新的 user message；发送按钮会切换为停止按钮，也可使用 `/stop` 中断当前运行
- **`/new` 对话压缩**：将对话摘要追加到每日记忆，然后清空上下文
- **Structured Memory（可选）**：启用 `structuredMemory` 后，Finish 阶段会异步抽取稳定偏好、项目上下文和长期事实，写入 workspace 下的 `structured_memory.json`，并记录 `structured_memory.audit.jsonl` 诊断轨迹；`/memory`、`/memory stats`、`/memory debug` 可查看状态与最近审计信息
- **Daily Reflection（可选）**：启用 `dailyReflection` 后，多步任务完成时会在 Finish 后台异步生成简短 reflection，追加到 workspace 下的 `memory/YYYY-MM-DD.md`；`/reflection`、`/reflection today`、`/reflection yesterday`、`/reflection list` 可查看状态和已过滤的 reflection 条目
- **更细粒度的 Token 统计**：Primary、Fast、Sub-Agent、Memory、Reflection、Context 六类模型角色都会分别累计 token；`/new` 压缩、自动上下文压缩、Structured Memory 和 Daily Reflection 的非流式调用也会计入 Usage
- **可关闭的 Slash Command 卡片**：聊天页中由斜杠命令返回的 `success`、`system`、`error` 卡片支持点击关闭；运行进度和自动压缩通知仍保持常驻提示
- **ReAct 显式状态机**：`match react_ctx.phase()` 驱动的 Analyze/Act/Observe/Finish 四阶段循环，`evaluate_finish()` 结构化完成判定，`auto_think_level()` 按循环深度动态调整推理预算
- **非破坏性 Observation 摘要**：大工具结果生成 WS 事件 + 系统提示注入，原始结果始终完整保留；错误工具标记 `[FAILED]` 并附带耗时
- **推理可见性控制**：默认开启 ReAct 阶段转换 WS 事件（`react_phase`），可通过 `/react on|off` 手动切换；浏览器前端会显示阶段切换，`done` 事件包含 `reason`（正常完成时 `complete` | `empty`，hard-cap 时 `hard_cap`）
- **结构化工具结果**：`ToolOutcome`（output + is_error + duration_ms），前缀式错误检测，schema 约束校验（required/type/range/length），`tool_result` WS 事件携带耗时和错误标记
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
mkdir -p "$HOME/.lingclaw/system-skills" "$HOME/.lingclaw/system-agents"
cp -R docs/reference/skills/. "$HOME/.lingclaw/system-skills/"
cp -R docs/reference/agents/. "$HOME/.lingclaw/system-agents/"

# 首次运行打开设置向导
lingclaw

# 服务管理
lingclaw start
lingclaw stop
lingclaw restart
lingclaw status
lingclaw mcp-check
lingclaw update
lingclaw doctor
lingclaw install
lingclaw install -d /path/to/source
lingclaw systemd-install        # Linux: 安装并启用 lingclaw.service
lingclaw health
lingclaw help
lingclaw --version
```

手动执行 `cargo install --path .` 时，必须同步部署 `static/`、`docs/reference/skills/` 和 `docs/reference/agents/`；否则首页可能返回 404，且内置 Skills / Sub-Agents 不可用。优先推荐直接使用 `bash scripts/install-linux.sh`。

服务启动后访问 http://127.0.0.1:18989 。

也可以只用环境变量：

```bash
# OpenAI
OPENAI_API_KEY=sk-xxx lingclaw

# Anthropic
ANTHROPIC_API_KEY=sk-ant-xxx LINGCLAW_MODEL=claude-sonnet-4-20250514 lingclaw

# Ollama
LINGCLAW_PROVIDER=ollama LINGCLAW_MODEL=qwen3 OLLAMA_API_BASE=http://127.0.0.1:11434 lingclaw
```

## Configuration

配置文件在 `~/.lingclaw/.lingclaw.json`，首次运行由设置向导自动写入；如需本地图片上传，可在向导里额外配置顶层 `s3`，也可以之后手动补充。若要配合 OpenAI/Anthropic 使用，`s3.endpoint` 对应的现签 URL 必须公网可达；私网或 localhost 网关仅推荐与 Ollama 搭配。

```json
{
  "settings": {
    "port": 18989,
    "execTimeout": 30,
    "toolTimeout": 30,
    "subAgentTimeout": 300,
    "maxLlmRetries": 2,
    "maxContextTokens": 32000,
    "maxOutputBytes": 51200,
    "maxFileBytes": 204800,
    "structuredMemory": false,
    "dailyReflection": false,
    "enableS3": true
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
      },
      "ollama": {
        "baseUrl": "http://127.0.0.1:11434",
        "apiKey": "",
        "api": "ollama",
        "models": [
          {
            "id": "qwen3",
            "name": "qwen3",
            "reasoning": true,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 8192,
            "compat": { "thinkingFormat": "ollama" }
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini",
        "fast": "openai/gpt-4o-mini",
        "sub-agent": "openai/gpt-4o-mini",
        "memory": "openai/gpt-4o-mini",
        "reflection": "openai/gpt-4o-mini",
        "context": "openai/gpt-4o-mini"
      }
    }
  },
  "s3": {
    "endpoint": "https://s3.us-east-1.amazonaws.com",
    "region": "us-east-1",
    "bucket": "my-bucket",
    "accessKey": "AKIA...",
    "secretKey": "your-secret-key",
    "prefix": "lingclaw/images/",
    "urlExpirySecs": 604800,
    "lifecycleDays": 14
  }
}
```

说明：

- 推荐使用 `provider/model` 格式引用模型
- 多个 provider 暴露同一 model ID 时，必须使用显式前缀
- 新配置应通过 `models.providers` 定义 provider 实例，并用 `agents.defaults.model.primary` 选择默认模型
- `structuredMemory` 默认为 `false`；启用后会在 Finish 阶段后台更新结构化记忆，并在后续 system prompt 中注入摘要；若配置了 `agents.defaults.model.memory` 或 `LINGCLAW_MEMORY_MODEL`，后台抽取优先使用该模型，否则回退到当前会话有效模型
- `dailyReflection` 默认为 `false`；启用后会在满足轮次和冷却条件时，于 Finish 后台生成 post-execution reflection，并追加到 `memory/YYYY-MM-DD.md`；若配置了 `agents.defaults.model.reflection` 或 `LINGCLAW_REFLECTION_MODEL`，reflection 优先使用该模型，否则回退到 memory 模型，再回退到当前会话有效模型
- 顶层 `s3` 为可选项；配置后聊天页会额外启用本地 JPEG/PNG 上传，上传对象以 object key 持久化，历史回放和 provider 请求都会即时重新现签 URL
- AWS S3 若使用官方 endpoint，建议使用与 `region` 对应的区域 host；设置向导留空 endpoint 时会自动默认到该区域地址
- OpenAI/Anthropic 直接使用现签 URL，因此 `s3.endpoint` 必须能被远端 provider 访问；私网、localhost 或 VPN-only 网关仅保证 Ollama 可用，因为 Ollama 路径会在本地预取并转成 base64
- 遗留字段 `settings.provider`、`settings.apiKey`、`settings.apiBase` 仍被读取以保持向后兼容，但 Setup Wizard 不再生成它们；新配置应省略这些字段
- `models.providers.*.api` 目前支持 `openai-completions`、`anthropic`、`ollama`
- Ollama 的 thinking / tool calling 依赖模型能力，推荐优先使用 `qwen3`、`gpt-oss`、`deepseek-r1` 等官方支持模型，而不是把任意本地模型都视为支持深度思考和工具调用
- 可选的 `mcpServers` 顶层对象可声明 MCP server，例如 `command`、`args`、`env`、`cwd`、`timeoutSecs`
- `mcpServers.*.cwd` 必须落在当前 session workspace 内；未设置 `timeoutSecs` 时默认继承 `toolTimeout`
- `start` / `restart` 的 MCP 预检使用受限的一次性探测，不会把预检进程保留为运行时 MCP 会话；`mcp-check` 会按配置的运行时超时做更深诊断
- `/mcp` 会在聊天页面列出当前已加载的 MCP servers；如果某个 server 失败，页面会显示失败原因，便于排查启动、命令解析或超时问题
- `/mcp refresh` 会清空当前 workspace 对应的 MCP 工具缓存、空闲会话和最近失败冷却状态，然后重新探测已启用 servers；运行时空闲 MCP 会话也会自动回收，`notifications/tools/list_changed` 会触发下一次工具发现时自动刷新
- 子代理执行和 `/agents` 展示都会在使用前按需预热 MCP 工具缓存，因此首次使用也能拿到最新的 MCP 工具列表

聊天页运行时交互说明：

- Agent 忙碌时，输入框中发送普通文本不会打断当前 tool/推理步骤，而是作为下一轮 Analyze 的纠偏输入
- Agent 忙碌时，发送按钮会变为停止按钮；点击后等价于发送 `/stop`
- Agent 忙碌时，只允许 `/stop`、`/tool`、`/reasoning` 立即执行；其他斜杠命令需等待当前运行结束

### Structured Memory

`structuredMemory` 是一个默认关闭的可选功能，用于维护一份**机器可读**的长期记忆，与人工编辑的 `MEMORY.md` 和 `memory/YYYY-MM-DD.md` 并存。

- 启用方式：在 `settings.structuredMemory` 中设为 `true`，或设置环境变量 `LINGCLAW_STRUCTURED_MEMORY=true`
- 存储位置：`~/.lingclaw/main/workspace/structured_memory.json`
- 审计文件：`~/.lingclaw/main/workspace/structured_memory.audit.jsonl`
- 更新时机：每轮回答完成后的 Finish 阶段，异步入队并做 3 秒 debounce，不阻塞主 agent loop
- 模型选择：优先使用 `agents.defaults.model.memory` 或环境变量 `LINGCLAW_MEMORY_MODEL`；未设置时回退到当前会话有效模型
- 提取来源：使用 user/assistant 对话内容，并附带 tool 调用名与 tool 结果首行摘要；会过滤自动生成的 `## Context Summary (auto-generated)` 压缩摘要
- 合并策略：模型返回缺失字段时保留旧值；显式 `null` 会清空 `user_context`；`facts` 返回时按完整列表替换
- 超时策略：memory 更新请求沿用 `toolTimeout` 预算，并设 30 秒下限，避免配置过小导致后台更新恒定超时
- 查看状态：`/memory` 显示当前摘要和 updater 运行状态，`/memory stats` 显示 updater 计数器，`/memory debug` 额外显示最近审计记录

### Daily Reflection

`dailyReflection` 是一个默认关闭的可选功能，用于把多步任务结束后的简短复盘写入每日记忆文件。它与 `structuredMemory` 不同：前者写的是面向人阅读的 daily log 条目，后者维护的是机器可读的 `structured_memory.json`。

- 启用方式：在 `settings.dailyReflection` 中设为 `true`，或设置环境变量 `LINGCLAW_DAILY_REFLECTION=true`
- 存储位置：`~/.lingclaw/main/workspace/memory/YYYY-MM-DD.md`
- 写入格式：reflection 会以 `## HH:MM Local — Reflection (...)` 形式追加到 daily memory 文件中，与 `/new` 写入的普通压缩摘要共存
- 触发条件：仅在完成阶段触发，默认至少需要 3 个 agent cycle，并受 10 分钟冷却限制；不阻塞主 agent loop
- 模型选择：优先使用 `agents.defaults.model.reflection` 或环境变量 `LINGCLAW_REFLECTION_MODEL`；未设置时回退到 `memory` 模型，再回退到当前会话有效模型
- 查看状态：`/reflection` 显示 feature 状态、运行时冷却信息和今天的 reflection 预览；`/reflection today` 与 `/reflection yesterday` 显示完整 reflection 条目；`/reflection list` 只列出实际包含 reflection 的日期文件
- 读取行为：`/reflection` 会从 `memory/YYYY-MM-DD.md` 中只提取带 `— Reflection` 头部的条目，自动忽略 `/new` 写入的普通摘要段落

## Environment Variables

| 变量 | 默认值 | 说明 |
|---|---|---|
| `OPENAI_API_KEY` | provider 配置或空 | OpenAI API Key |
| `ANTHROPIC_API_KEY` | provider 配置或 `OPENAI_API_KEY` | Anthropic API Key |
| `OLLAMA_API_KEY` | provider 配置或空 | Ollama API Key，可留空用于本地实例 |
| `LINGCLAW_PROVIDER` | 自动检测 | 强制指定 `openai`、`anthropic` 或 `ollama` |
| `OPENAI_API_BASE` | `https://api.openai.com/v1` | 备用 API Base |
| `OLLAMA_API_BASE` | `http://127.0.0.1:11434` | Ollama API Base |
| `LINGCLAW_MODEL` | `gpt-4o-mini` | 默认模型 |
| `LINGCLAW_PORT` | `18989` | HTTP 端口 |
| `LINGCLAW_EXEC_TIMEOUT` | `30` | Shell 命令超时（秒） |
| `LINGCLAW_TOOL_TIMEOUT` | `30` | 非 shell 的 Act 阶段工具超时（秒），不适用于子代理 |
| `LINGCLAW_SUB_AGENT_TIMEOUT` | `300` | 子代理总执行超时（秒，0=无限，仅 max_turns 和 /stop 限制） |
| `LINGCLAW_MAX_LLM_RETRIES` | `2` | LLM API 瞬态错误（429/5xx/连接/超时）的 HTTP 级重试次数 |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | `32000` | 默认上下文 token 预算 |
| `LINGCLAW_FAST_MODEL` | 无 | 简单首轮查询使用的轻量模型（如 `openai/gpt-4o-mini`） |
| `LINGCLAW_SUB_AGENT_MODEL` | 无 | 子代理委托任务使用的模型（如 `openai/gpt-4o-mini`） |
| `LINGCLAW_MEMORY_MODEL` | 无 | structured memory 后台抽取优先使用的模型（如 `openai/gpt-4o-mini`） |
| `LINGCLAW_REFLECTION_MODEL` | 无 | daily reflection 后台生成优先使用的模型（如 `openai/gpt-4o-mini`） |
| `LINGCLAW_CONTEXT_MODEL` | 无 | 上下文自动压缩优先使用的模型（如 `openai/gpt-4o-mini`），未设置时回退到当前会话有效模型 |
| `LINGCLAW_STRUCTURED_MEMORY` | `false` | 启用后台结构化记忆提取与 prompt 注入 |
| `LINGCLAW_DAILY_REFLECTION` | `false` | 启用后台 daily reflection 生成与 `/reflection` 查看能力 |

## Slash Commands

| 命令 | 说明 |
|---|---|
| `/new` | 压缩对话到每日记忆，清空上下文 |
| `/model [name]` | 查看可用模型或切换当前会话模型 |
| `/think [level]` | 设置思维模式：`auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh` |
| `/react [on\|off]` | 切换 ReAct 阶段可见性（默认开启；启用后每次阶段转换发送 `react_phase` WS 事件） |
| `/tool [on\|off]` | 切换工具卡片显示；该设置会持久化到当前 session 的视图状态 |
| `/reasoning [on\|off]` | 切换 reasoning 面板显示；该设置会持久化到当前 session 的视图状态 |
| `/stop` | 中断当前运行中的 agent；聊天页停止按钮与该命令等价 |
| `/skills` | 列出可用工具和已安装的 Skills（含来源标签） |
| `/skills-system [install\|uninstall <pattern>]` | 列出系统内置 Skills 状态；`install`/`uninstall` 子命令可运行时启用/禁用 Skill 或 Skill 组（如 `anthropics`、`anthropics/pdf`） |
| `/skills-global` | 仅列出全局 Skills（`~/.lingclaw/skills/`） |
| `/skills-session` | 仅列出当前 session Skills（workspace `skills/`） |
| `/agents` | 列出已发现的子代理（含来源标签）以及每个子代理当前可用的有效工具列表（含 MCP 工具） |
| `/status` | 显示模型、provider、上下文估算、最大输出 token、思维级别，token 数值按 K/M 显示 |
| `/system-prompt` | 输出当前会话的新鲜系统提示词，以及该系统提示词按当前 provider 估算的 token 开销 |
| `/mcp [refresh]` | 查看当前已加载的 MCP server 状态；加上 `refresh` 时强制刷新工具缓存并重建运行时 MCP 会话 |
| `/usage` | 显示当前 session 的累计输入、输出、总 token 估算用量，以及今日输入、输出、总量估算；单会话模式下同时显示主会话今日总 token 估算，按 K/M 显示 |
| `/clear` | 清空消息但保留系统提示 |
| `/memory [stats\|debug]` | 查看当前 structured memory 摘要与 updater 状态；`stats` 仅显示运行状态，`debug` 额外显示最近审计记录 |
| `/reflection [today\|yesterday\|list]` | 查看当前 daily reflection 状态与 reflection 条目；默认显示 feature 状态、冷却信息和今天的 reflection 预览，`list` 只列出实际包含 reflection 的日期文件 |
| `/help` | 命令帮助 |

聊天页里由斜杠命令生成的 `success`、`system`、`error` 卡片支持点击右上角关闭；`progress`、`context_pruned`、`context_compressed`、`context_compress_failed` 这类运行态通知不提供关闭按钮。

Settings → Usage 页面除了现有今日/累计图表外，还会显示按 `Primary`、`Fast`、`Sub-Agent`、`Memory`、`Reflection`、`Context` 划分的 Token Breakdown。对于升级前已经存在的旧会话，角色级累计值会从 0.5.6 开始逐步建立；旧快照中仍保留的 provider 数据会继续在历史图表里展示。

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
| `task` | 委托任务给子代理（当发现代理时动态注册）|

## Skills

Skills 是可安装的知识模块，教会 AI 助手如何完成特定领域的任务。每个 Skill 是一个独立目录，包含 `SKILL.md` 文件和可选的参考资源。

### 三层来源

Skills 从三个目录分层加载，后加载的同名 Skill 覆盖先前的：

| 层级 | 目录 | 说明 |
|------|------|------|
| **System** | `docs/reference/skills/` 或 `~/.lingclaw/system-skills/` | 随程序分发的内置 Skills；安装时自动部署到 `~/.lingclaw/system-skills/` |
| **Global** | `~/.lingclaw/skills/` | 跨 session 共享的全局 Skills |
| **Session** | `~/.lingclaw/main/workspace/skills/` | 主会话专属 Skills |

### 结构

```text
skills/
├── my-skill/
│   ├── SKILL.md          # 必需：YAML frontmatter + 指令正文
│   ├── references/       # 可选：参考文档
│   └── scripts/          # 可选：辅助脚本
└── another-skill/
    └── SKILL.md
```

### SKILL.md 格式

```markdown
---
name: my-skill
description: 描述这个 Skill 做什么以及何时触发
---

# 指令正文

详细步骤、示例、规范等...
```

### 工作原理

- **发现**：系统自动扫描三层目录下的 `skills/*/SKILL.md`
- **元数据注入**：Skill 名称、来源标签和描述在每轮对话的系统提示中呈现（Level 1：始终可见）
- **按需加载**：AI 在任务匹配时通过 `read_file` 读取完整 SKILL.md 内容（Level 2）。System 和 Global Skills 使用虚拟路径（如 `system://skills/anthropics/pdf/SKILL.md`、`~/.lingclaw/skills/xxx/SKILL.md`），`read_file`、`list_dir`、`search_files` 均可透明访问
- **资源引用**：Skill 目录中的参考文件按需读取（Level 3）
- **去重**：同名 Skill 按 System → Global → Session 顺序加载，后加载的覆盖先前的
- **运行时管理**：
  - `/skills` — 列出所有 Skills（含来源标签）和工具
  - `/skills-system` — 列出系统内置 Skills 状态（loaded/disabled）
  - `/skills-system install <pattern>` — 重新启用之前禁用的 Skill
  - `/skills-system uninstall <pattern>` — 禁用 Skill（支持组级如 `anthropics` 或单个如 `anthropics/pdf`）
  - `/skills-global` — 仅列出全局 Skills
  - `/skills-session` — 仅列出当前 session Skills
- **部署**：`lingclaw install` 和 `lingclaw update` 会自动将 `docs/reference/skills/` 复制到 `~/.lingclaw/system-skills/`，并将 `docs/reference/agents/` 复制到 `~/.lingclaw/system-agents/`，确保系统 Skills / Sub-Agents 在安装后可用

### 兼容性

SKILL.md 的 YAML frontmatter 格式兼容 [Agent Skills 规范](https://agentskills.io)。你可以从 [anthropics/skills](https://github.com/anthropics/skills) 仓库获取社区 Skill 并放入任意层级的 `skills/` 目录。

## Sub-Agents

子代理是可委托的专用任务执行器。主 Agent 通过 `task` 工具将子任务分派给子代理，子代理在独立的 ReAct 循环中执行，完成后将结果返回给主 Agent。

### 三层来源

子代理从三个目录分层加载，后加载的同名代理覆盖先前的：

| 层级 | 目录 | 说明 |
|------|------|------|
| **System** | `docs/reference/agents/` | 随程序分发的内置子代理 |
| **Global** | `~/.lingclaw/agents/` | 跨 session 共享的全局子代理 |
| **Session** | `~/.lingclaw/main/workspace/agents/` | 主会话专属子代理 |

### 内置子代理

| 名称 | 用途 |
|------|------|
| **explore** | 快速只读代码库探索和问答 |
| **researcher** | 深度研究，综合多源信息 |
| **coder** | 代码实现与修改 |
| **reviewer** | 代码审查与质量检查 |

### AGENT.md 格式

```markdown
---
name: my-agent
description: 描述这个子代理做什么
max_turns: 15               # 可选：最大 ReAct 轮数（默认 15）
tools:
  allow: ["read_file", "list_dir", "search_files"]   # 白名单模式
  # deny: ["exec", "write_file"]                      # 或黑名单模式
---

# 系统提示正文

详细的行为指令...
```

`tools.allow` / `tools.deny` 同时作用于内置工具和 `mcp__...` 形式暴露出的 MCP 工具名。若启用了 MCP server，可先用 `/agents` 查看当前子代理的有效工具列表，再决定是否在 `AGENT.md` 中做精确白名单或黑名单控制。

### 模型选择

所有子代理统一使用配置文件中的模型设置：

1. **`agents.defaults.model.sub-agent`** — 全局子代理模型配置（JSON 配置或 `LINGCLAW_SUB_AGENT_MODEL` 环境变量）
2. **`agents.defaults.model.primary`** — 主模型（兜底）

`AGENT.md` 中即使存在遗留的 `model` 字段，当前版本也会忽略，不参与运行时模型选择。

### 工作原理

- **发现**：系统自动扫描三层目录下的 `agents/*/AGENT.md`，解析 YAML frontmatter
- **动态注册**：当发现至少一个子代理时，`task` 工具会被动态添加到模型工具列表
- **隔离执行**：子代理拥有独立的消息历史、过滤后的工具集、独立的 ReAct 循环；工具集同时可包含内置工具和 MCP 工具
- **超时与安全**：子代理总执行时间受 `subAgentTimeout`（默认 300s）限制，内部各工具保留各自超时；`max_turns` 有 50 轮硬上限；`/stop` 和断开连接可随时取消
- **Hook 集成**：子代理的工具执行经过 BeforeToolExec / AfterToolExec Hook 链，Reject 事件会转发给父 Agent
- **递归阻断**：`task` 工具始终被排除在子代理的工具集之外，防止无限委托
- **事件流**：`task_started`、`task_progress`、`task_tool`、`task_completed`、`task_failed` 事件实时流向前端
- **查看代理**：`/agents` 列出所有已发现的子代理、来源以及当前过滤后的有效工具列表

---

## Architecture

### 总体视图

```text
┌──────────────────────────────────────────────────────────────────┐
│                         Browser (static/)                        │
│   index.html  ·  js/ (ES modules)  ·  css/ (modular CSS)         │
└────────────────────────┬─────────────────────────────────────────┘
                         │ WebSocket /ws
┌────────────────────────▼─────────────────────────────────────────┐
│                     Axum HTTP Server                             │
│   GET /api/health · GET /api/sessions · POST /api/shutdown       │
│   GET/PUT /api/config · POST /api/config/test-model              │
│   POST /api/config/test-mcp · GET /api/usage                     │
│   GET /ws (WebSocket upgrade)                                    │
└────────────────────────┬─────────────────────────────────────────┘
                         │
┌────────────────────────▼─────────────────────────────────────────┐
│                    Connection Layer (Loop)                        │
│   handle_socket() · handle_command() · session persistence       │
│   active_connections · session_clients · live_rounds             │
│   memory queue · replay/rebind · cooperative shutdown            │
└───────┬──────────────────┬───────────────────┬───────────────────┘
        │                  │                   │
┌───────▼───────┐  ┌───────▼────────┐  ┌──────▼────────┐
│  Agent Loop   │  │  Session Store │  │  Config       │
│  ReAct FSM    │  │  主会话持久化    │  │  模型路由      │
│  ≤200 rounds  │  │  主会话工作区    │  │  环境变量回退   │
└───┬───────┬───┘  └────────────────┘  └───────────────┘
    │       │
┌───▼───┐ ┌─▼──────────────────┐
│ Skill │ │      CLI           │
│ Layer │ │    (Tools)         │
└───┬───┘ └───┬────────────────┘
    │         │
┌───▼─────────▼────────────────────────────────────────────────────┐
│                      Provider Layer                               │
│   call_llm_stream() → OpenAI SSE / Anthropic SSE / Ollama NDJSON │
│   ResolvedModel · thinking/reasoning 参数映射                      │
│   tool_definitions() · tool_definitions_anthropic() · tool_definitions_ollama() │
└──────────────────────────────────────────────────────────────────┘
```

### 三层架构：Skill + CLI + Loop

| 层 | 职责 | 代码位置 |
|---|---|---|
| **Skill** | LLM 推理、系统提示构建、上下文裁剪、token 估算、思维模式、结构化记忆注入 | `src/main.rs`（`build_system_prompt`, `prune_messages`, `estimate_tokens`）、`src/providers.rs`（流式调用）、`src/prompts.rs`（模板加载）、`src/memory.rs`（结构化记忆） |
| **CLI** | 工具注册/分发/执行、路径沙盒、危险命令检测、SSRF 防护 | `src/tools/mod.rs`（注册表）、`src/tools/fs.rs`（文件工具）、`src/tools/net.rs`（网络工具）、`src/tools/exec.rs`（执行工具） |
| **Loop** | WebSocket 处理、会话生命周期、斜杠命令、持久化、HTTP API | `src/main.rs`（`handle_socket`, `handle_command`, session 管理） |

### ReAct 状态机

Agent Loop 采用显式的 **ReAct 风格有限状态机**，将经典 ReAct 的 Thought → Action → Observation 循环转化为结构化阶段控制：

运行中的用户干预不会强制截断当前阶段。LingClaw 会在阶段边界收集用户追加的普通文本，并在下一次 `Analyze` 前将其作为新的 user message 注入上下文；如果需要立刻停止当前轮次，使用 `/stop` 或聊天页停止按钮。

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

- **不回退到文本协议**：保留 OpenAI/Anthropic/Ollama 原生结构化 tool calling，不使用文本版 `Action: tool_name\nAction Input: {...}` 解析
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
  │    │    ├─ execute_tool() × N (含 task 工具 → 子代理委托)
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
  │         ├─ 运行 OnFinish hooks
  │         ├─ enqueue structured memory update（启用时）
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
├── main.rs            (~1750 行) — 共享类型, WebSocket/HTTP 处理, 系统提示构建, 安全检查
├── runtime_loop.rs    (~1900 行) — 阶段执行循环, 工具进度, 运行取消, 干预持久化, orchestrate 执行
│   └── socket_input.rs (~400 行) — socket 空闲/忙碌输入辅助
├── agent.rs           (~420 行)  — AgentPhase 状态机, FinishReason, evaluate_finish, auto_think_level, Observation 摘要
├── commands.rs        (~1460 行) — 斜杠命令处理器 (handle_command, /skills-system install/uninstall 等)
├── cli.rs             (~2220 行) — CLI 子命令, 设置向导, PATH/systemd, 安装/更新, system skills 部署, doctor 就绪检查
├── config.rs          (~840 行)  — Provider/Config/JsonConfig 结构体, 模型解析, 超时加载
├── context.rs         (~350 行)  — token 估算, 上下文预算, 裁剪, 用量格式化
├── providers.rs       (~1640 行) — OpenAI/Anthropic/Ollama 调用, 流式解析, 推理模式, prompt caching
├── prompts.rs         (~870 行)  — 提示文件初始化/加载, bootstrap baseline, Skills 发现/注入, 虚拟路径解析
├── hooks.rs           (~660 行)  — HookRegistry, AgentHook trait, 自动压缩上下文 hook
├── memory.rs          (~970 行)  — structured_memory.json 读写, MemoryUpdateQueue, prompt 注入, /memory 状态
├── image_uploads.rs   (~670 行)  — S3 签名/上传, PNG/JPEG 校验, 生命周期管理, 附件令牌签发
├── session_admin.rs   (~10 行)   — 全局用量统计 (仅主会话)
├── session_store.rs   (~420 行)  — 会话持久化, 迁移, 磁盘 I/O
├── socket_sync.rs     (~90 行)   — WebSocket 会话声明, 断线监听, 重绑定
├── socket_tasks.rs    (~130 行)  — WebSocket 读写任务
└── tools/
    ├── mod.rs         (~870 行)  — ToolSpec 注册表, tool_definitions(), execute_tool(), ToolOutcome, 参数校验, orchestrate/task 定义
    ├── fs.rs          (~350 行)  — read_file, write_file, patch_file, delete_file, list_dir, search_files + 虚拟 skill 路径
    ├── net.rs         (~190 行)  — http_fetch, check_ssrf, is_private_ip
    ├── exec.rs        (~60 行)   — exec (shell), think (scratchpad)
    └── mcp.rs         (~1400 行) — stdio MCP 工具发现/执行桥接, 会话缓存, preflight
├── subagents/
│   ├── mod.rs         (~260 行)  — SubAgentSpec, ToolPermissions, AgentSource, catalog 渲染, 工具过滤（含 MCP）
│   ├── executor.rs    (~890 行)  — 隔离 mini-ReAct 执行循环, Hook 集成, MCP 工具调度, 父级事件流
│   ├── discovery.rs   (~320 行)  — 三层发现 (system/global/session), YAML frontmatter 解析
│   └── orchestrator.rs (~770 行) — DAG 多代理编排引擎, 分层并行执行, 结果插值, 事件流

static/
├── index.html                  — 主页面
├── css/                        — 模块化样式 (base, layout, chat, panels, pages, responsive)
└── js/                         — 前端 ES Modules
    ├── main.js                 — 入口模块（流式渲染、懒加载历史、智能滚动、版本 badge）
    ├── constants.js            — 常量
    ├── state.js                — 集中状态 + DOM refs
    ├── utils.js                — 纯工具函数
    ├── scroll.js               — 滚动/视口管理
    ├── markdown.js             — Markdown/KaTeX 管线
    ├── socket.js               — WebSocket 连接
    ├── images.js               — 图片附件 + 上传
    ├── input.js                — 输入处理 + 历史导航
    ├── mobile.js               — 移动端菜单
    ├── settings.js             — Settings 页面（配置编辑、Provider 测试、MCP 测试）
    ├── usage.js                — Usage 页面（Token 用量统计 + 图表）
    ├── handlers/stream.js      — 流式响应处理
    └── renderers/              — UI 渲染模块 (chat, tools, react-status, timeline)

docs/reference/templates/       — 7 个提示模板文件 (BOOTSTRAP/AGENTS/IDENTITY/SOUL/USER/TOOLS/MEMORY.md)
docs/reference/skills/          — 17 个系统内置 Skills (安装时部署到 ~/.lingclaw/system-skills/)
docs/reference/agents/          — 4 个内置子代理 (explore, researcher, coder, reviewer)

src/tests/                      — 模块测试文件 (~13600 行)
```

### 核心数据结构

```rust
enum Provider { OpenAI, Anthropic, Ollama }

struct Config {
    api_key, api_base, model, provider,
    providers: HashMap<String, JsonProviderConfig>,
  port, max_context_tokens, exec_timeout, tool_timeout,
  max_output_bytes, max_file_bytes, structured_memory,
}

struct Session {
    id, name, messages: Vec<ChatMessage>,
    created_at, updated_at, tool_calls_count,
    model_override: Option<String>,
    think_level: String,       // "auto"|"off"|"minimal"|"low"|"medium"|"high"|"xhigh"
    workspace: PathBuf,        // ~/.lingclaw/{id}/workspace/
    show_tools: bool,
    show_reasoning: bool,
    show_react: bool,
    disabled_system_skills: HashSet<String>,  // 运行时禁用的系统 Skill 模式
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
  thinking_format: Option<String>,  // "qwen"|"openai"|"anthropic"|"ollama"
    max_tokens: Option<u64>,
}

struct StructuredMemory {
  user_context: Option<String>,
  facts: Vec<MemoryFact>,
  updated_at: u64,
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

三 Provider 支持，统一的调用接口：

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
        ├─ resolved.provider == Anthropic
        │    └─ call_llm_stream_anthropic()
        │         ├─ convert_messages_to_anthropic()
        │         ├─ tool_definitions_anthropic()
        │         ├─ think_level → budget_tokens 映射
        │         └─ SSE 流解析 → WebSocket 转发
        │
        └─ resolved.provider == Ollama
          └─ call_llm_stream_ollama()
            ├─ convert_messages_to_ollama()
            ├─ tool_definitions_ollama()
            ├─ think_level → think 映射
            └─ NDJSON 流解析 → WebSocket 转发
```

think_level 映射：

| level | OpenAI reasoning_effort | Anthropic budget_tokens | Ollama think |
|---|---|---|---|
| off | — | — | `false`（GPT-OSS 例外，无法完全关闭） |
| minimal | low | 1024 | `true`；GPT-OSS 映射到 `low` |
| low | low | 4096 | `true`；GPT-OSS 映射到 `low` |
| medium | medium | 10240 | `true`；GPT-OSS 映射到 `medium` |
| high | high | 16384 | `true`；GPT-OSS 映射到 `high` |
| xhigh | high | 32768 | `true`；GPT-OSS 映射到 `high` |
| auto | model 支持 reasoning? medium : off | 同左 | model 支持 reasoning? `true` : off |

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
├── system-skills/          — 安装时部署的系统 Skills (从 docs/reference/skills/ 复制)
├── system-agents/          — 安装时部署的系统子代理 (从 docs/reference/agents/ 复制)
├── skills/                 — 全局 Skills (跨 session 共享)
├── sessions/
│   └── main.json           — 主会话存档
├── main/workspace/         — 主会话工作区
│   ├── AGENTS.md           — 核心代理行为
│   ├── IDENTITY.md         — 身份信息
│   ├── SOUL.md             — 高层推理规则
│   ├── USER.md             — 用户特定行为
│   ├── TOOLS.md            — 工具使用指南
│   ├── MEMORY.md           — 持久记忆指南
│   ├── structured_memory.json  — 机器可读结构化记忆（启用时生成）
│   ├── skills/             — session 专属 Skills
│   ├── agents/             — session 专属子代理
│   └── memory/
│       └── 2026-03-17.md   — 每日记忆
```

提示加载模式：

| 模式 | 条件 | 加载文件 |
|---|---|---|
| **Bootstrap** | `BOOTSTRAP.md` 存在 | `BOOTSTRAP.md + AGENTS.md` |
| **Normal** | `BOOTSTRAP.md` 不存在 | `AGENTS.md + IDENTITY.md + USER.md + SOUL.md + TOOLS.md`，然后加载当前 session 的 `MEMORY.md` + 今日/昨日记忆 |

关键不变式：
- 当 `IDENTITY.md` 和 `USER.md` 中的关键字段已被有效填写后，后端会自动删除 `BOOTSTRAP.md` 并切换到 Normal 模式
- `/new` 只压缩对话 + 写入记忆 + 清空上下文，不重建 session
- 重连不重建 `BOOTSTRAP.md`
- 启用 `structuredMemory` 时，system prompt 会额外注入 `structured_memory.json` 的摘要，但不会替代人工维护的 `MEMORY.md`
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
| `start` | 新一轮回复开始 |
| `delta` | 流式文本片段 |
| `thinking_start` | 思维模式开始 |
| `thinking_delta` | 思维流式片段 |
| `thinking_done` | 思维模式结束 |
| `tool_call` | 工具调用开始 |
| `tool_result` | 工具执行结果（含 `duration_ms`、`is_error`） |
| `done` | 响应完成 |
| `react_phase` | ReAct 阶段转换（默认启用，可通过 `/react off` 关闭） |
| `task_started` | 子代理开始执行 |
| `task_progress` | 子代理 ReAct 周期进度 |
| `task_tool` | 子代理工具调用（含 `agent`、`tool`、`id`） |
| `task_completed` | 子代理完成（含 `cycles`、`tool_calls`、`duration_ms`） |
| `task_failed` | 子代理失败（含 `error`、`cycles`、`tool_calls`、`duration_ms`） |
| `observation` | 非破坏性工具结果摘要 |
| `context_pruned` | 上下文裁剪通知（含 `removed_count`） |
| `progress` | 命令处理中（不清除忙碌状态） |
| `success` | 命令成功（成功样式） |
| `system` | 中性系统消息 |
| `error` | 错误消息 |
| `session` | 当前主会话已初始化或刷新 |

## HTTP API

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/health` | GET | 健康检查（返回 `version`、`model`、`sessions`） |
| `/api/sessions` | GET | 返回主会话信息 |
| `/api/client-config` | GET | 返回前端配置（上传 token、S3 能力标记等） |
| `/api/config` | GET | 读取原始 JSON 配置文件（含解析错误回退） |
| `/api/config` | PUT | 校验并保存 JSON 配置文件（原子写入 + 备份恢复） |
| `/api/config/test-model` | POST | 测试模型 Provider 连接（发送 "Hi" 并检查响应） |
| `/api/config/test-mcp` | POST | 测试 MCP Server 连接（spawn + tools/list） |
| `/api/usage` | GET | 返回 Token 用量统计（今日/累计/来源），并额外提供 `daily_roles`、`total_providers`、`total_roles`，以及按天拆分后的 `usage_history[].providers` / `usage_history[].roles` |
| `/api/upload-images` | POST | 上传本地图片到 S3（需启用 S3 配置） |
| `/api/shutdown` | POST | 认证的本地关停端点（CLI 使用） |
| `/ws` | GET | WebSocket 升级端点 |

## Session Workspace

主会话拥有独立工作区 `~/.lingclaw/main/workspace/`，包含以下提示文件：

| 文件 | 用途 |
|---|---|
| `BOOTSTRAP.md` | 初始引导指令 |
| `AGENTS.md` | 核心代理行为 |
| `IDENTITY.md` | 身份/人格信息 |
| `SOUL.md` | 高层推理规则 |
| `USER.md` | 用户特定行为指导 |
| `TOOLS.md` | 工具使用指导 |
| `MEMORY.md` | 持久记忆指导 |

每个工作区还有 `memory/` 子目录，存放 `memory/YYYY-MM-DD.md` 每日日志。
启用 `structuredMemory` 后，还会在同目录生成 `structured_memory.json`，供后台异步记忆抽取和后续 prompt 注入使用。

## 鸣谢

- 感谢 openclaw、claude code、deer-flow、opencode
- 感谢 vide-coding 伙伴：GPT-5.4、Claude Opus 4.6、Doubao-Seedream-4.5
- 感谢 GitHub Copilot、VS Code
- 感谢 豆包、千问
- 感谢时间

## License

本项目采用 MIT License。
完整条款见 [LICENSE](LICENSE)。
