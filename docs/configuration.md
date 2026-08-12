# LingClaw 配置指南

[简体中文](configuration.md) · [English](configuration.en.md) · [返回 README](../README.md)

LingClaw 的配置文件位于 `~/.lingclaw/.lingclaw.json`。首次启动的 Setup Wizard 可以创建基础配置；之后优先通过 Web UI 的 Settings 修改。规范的新式配置示例见 [`.lingclaw.json.example`](../.lingclaw.json.example)；为避免在新安装中继续传播旧式单 Provider 配置，样例不包含遗留兼容字段。

## 首次配置

普通 Agent 对话需要两个条件同时成立：

1. `models.providers` 中至少有一个运行时可用的 Provider 和 Model。
2. `agents.defaults.model.primary` 或当前 Session `/model` 引用其中一个有效模型。

LingClaw 内部保留兼容回退值用于解析旧配置，但它不会被视为“用户已配置模型”，也不会解锁普通发送按钮。

最小 OpenAI-compatible 配置：

```json
{
  "models": {
    "providers": {
      "openai": {
        "baseUrl": "https://api.openai.com/v1",
        "apiKey": "${OPENAI_API_KEY}",
        "api": "openai-completions",
        "models": [
          {
            "id": "gpt-4o-mini",
            "name": "gpt-4o-mini",
            "reasoning": false,
            "input": ["text", "image"],
            "contextWindow": 128000,
            "maxTokens": 16384
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini"
      }
    }
  }
}
```

设置环境变量后启动：

```powershell
$env:OPENAI_API_KEY = "sk-..."
lingclaw restart
```

```bash
export OPENAI_API_KEY="sk-..."
lingclaw restart
```

Settings 保存配置时会先校验、原子写入并应用新的运行时快照。直接在磁盘编辑文件不会自动热加载；编辑完成后重启 LingClaw，或在 Settings 中重新保存。

## settings

常用字段：

| 字段 | 默认值 | 说明 |
|---|---:|---|
| `port` | `18989` | 本地 HTTP/WebSocket 端口 |
| `execTimeout` | `30` | `exec` 超时，秒 |
| `toolTimeout` | `30` | 非 shell 工具和部分辅助调用超时，秒 |
| `subAgentTimeout` | `300` | Sub-agent 总超时；`0` 表示不使用时间上限 |
| `maxLlmRetries` | `2` | 429、5xx、连接和超时类错误的重试次数，最大 10 |
| `maxContextTokens` | `32000` | 未被模型字段覆盖时的上下文预算 |
| `maxOutputBytes` | `51200` | 工具输出字节上限 |
| `maxFileBytes` | `204800` | 文件工具字节上限 |
| `openaiStreamIncludeUsage` | `false` | 请求 OpenAI-compatible stream usage |
| `anthropicPromptCaching` | `false` | 启用 Anthropic prompt caching 标记 |
| `structuredMemory` | `false` | 启用 Structured Memory |
| `dailyReflection` | `false` | 启用 Daily Reflection |
| `enableStateDigest` | `true` | 启用工具观察后的工作状态摘要 |
| `enableTaskPlan` | `false` | 启用普通 Execute run 的“自动执行提纲”；Plan-only 与已批准计划执行期间自动抑制 |
| `enableGroups` | `false` | Group chat 总开关；关闭时 WebUI、TUI、API、WebSocket 与 Agent Group 操作均不可用，已有 Group 数据保留 |
| `enableS3` | 存在 `s3` 时启用 | 总开关，可覆盖 S3 配置存在状态 |

服务始终绑定 `127.0.0.1:<port>`。修改防火墙不会让 LingClaw 直接监听外网；远程访问应使用受保护的反向代理或 SSH tunnel。

Group chat 是显式 opt-in 功能。旧配置缺少 `enableGroups` 时也按 `false` 处理；在 Console → General 或 TUI Settings 中开启后立即热更新。关闭不会删除 Group、成员、历史或投票，重新开启即可恢复。直接编辑 `.lingclaw.json` 仍需重启进程。

## Providers

Provider 名称是 `models.providers` 下的自定义键。推荐用短、稳定、只含安全字符的名称，并始终以 `provider/model` 引用模型。

JSON 中的 `baseUrl` 和 `apiKey` 都是必填字段，LingClaw 不会在字段缺失时自动补全 Base URL。无需鉴权的本地端点（例如默认 Ollama）也必须显式写入 `"apiKey": ""`。`api` 可以省略，此时默认为 `openai-completions`。

| `api` | 常用 Base URL（需显式填写） | 上游协议 |
|---|---|---|
| `openai-completions` | `https://api.openai.com/v1` | `POST /v1/chat/completions` |
| `openai-responses` | `https://api.openai.com/v1` | `POST /v1/responses` |
| `anthropic` | `https://api.anthropic.com` | Anthropic Messages |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta` | Gemini generateContent |
| `ollama` | `http://127.0.0.1:11434` | Ollama chat |

`baseUrl` 与 `apiKey` 支持完整值为 `${ENV_NAME}` 的占位符。占位符不存在时，该 Provider 在运行时不可用；配置仍保留在磁盘，便于修复环境后恢复。

```json
{
  "baseUrl": "${OPENAI_COMPAT_BASE_URL}",
  "apiKey": "${OPENAI_COMPAT_API_KEY}",
  "api": "openai-completions",
  "models": []
}
```

Settings → Models 的 Test 操作会发送一个轻量请求验证端点。它会产生真实 Provider 请求和可能的少量 Token 用量。

### Model 字段

| 字段 | 说明 |
|---|---|
| `id` | 上游模型 ID，必填 |
| `name` | 展示名称；省略时使用 ID |
| `reasoning` | 模型是否支持推理 effort |
| `effort` | 可选的 Effort 级别与默认值；仅影响当前模型允许用户选择的范围 |
| `input` | `text` 和可选 `image` 能力声明 |
| `contextWindow` | 上下文窗口 Token |
| `maxTokens` | 单次最大输出 Token |
| `cost` | 兼容性定价元数据；当前 Usage 只统计 Token，不计算金额 |
| `compat` | Provider/模型兼容覆盖 |

`input: ["text", "image"]` 是 LingClaw 暴露图片附件和工具图片能力的必要条件之一，不代表上游端点一定接受所有组合。OpenAI-compatible Chat 在明确拒绝图片/tool 内容时会移除工具图片并重试一次；普通鉴权、限流或 schema 错误不会触发该降级。

### Thinking Effort

推理模型可以显式限制输入区与 `/think` 可选择的 Effort，并指定切换到该模型时使用的默认值：

```json
{
  "id": "reasoning-model",
  "reasoning": true,
  "effort": {
    "levels": ["auto", "low", "medium", "high"],
    "default": "medium"
  }
}
```

可配置值按固定顺序为 `auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`。`levels` 不能为空或重复，`default` 必须包含在 `levels` 中；除 `off` 外的值要求 `reasoning: true`。关闭 Reasoning 会清除非 `off` 的 Effort 配置。

旧版推理模型没有 `effort` 时继续兼容完整集合并默认使用 `auto`；非推理模型等效为 `levels: ["off"]`。输入区会把模型与 Effort 作为当前 Session 的一个原子选择持久化；切换模型时，若原 Effort 不受支持则使用目标模型的 `default`。Settings 热重载移除了当前 Effort 后也会执行相同规范化并保存。

### Thinking compatibility

OpenAI-compatible 模型可以设置 `compat.thinkingFormat`。Settings 下拉框提供：

- `openai`
- `qwen`
- `doubao`
- `deepseek-v4`
- `ollama`
- `gpt-oss`
- `ollama-gpt-oss`

未设置时使用协议默认行为；旧配置中的自定义值会被保留。`openai-responses` 还支持：

```json
{
  "compat": {
    "reasoning": {
      "summary": "auto"
    }
  }
}
```

模型配置定义用户可选范围，`compat.thinkingFormat` 则负责把已选 Effort 映射到上游方言。不同模型对 `off`、`minimal`、`xhigh` 和 `max` 的支持并不一致；`/status` 显示当前解析后的模型与 think 状态。

## Agent 模型路由

```json
{
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini",
        "fast": "openai/gpt-4o-mini",
        "sub-agent": "openai/gpt-4o-mini",
        "sub-agent-reviewer": "anthropic/claude-sonnet",
        "memory": "openai/gpt-4o-mini",
        "reflection": "openai/gpt-4o-mini",
        "context": "openai/gpt-4o-mini"
      }
    }
  }
}
```

- Settings → Agents 的“全部切换为”会一次更新所有默认角色，不修改 Session `/model` 覆盖。
- `sub-agent-<name>` 优先于通用 `sub-agent`。
- 未单独配置 Fast/Memory/Reflection/Context 时，各调用按运行时规则回退到当前有效模型。
- 多个 Provider 含有相同纯 Model ID 时，必须使用 `provider/model` 消除歧义。
- 删除或重命名被引用的 Model 前，应先更新 Agent roles 和 Session overrides。

## MCP

`mcpServers` 只声明可发现的 server catalog；配置 server 不会自动把工具注入所有 Session。保存后，在 Settings → MCP 为当前 Session 开启 server 和具体 tools。

### stdio

```json
{
  "mcpServers": {
    "local-tools": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "example-mcp-server"],
      "env": {
        "EXAMPLE_TOKEN": "${EXAMPLE_TOKEN}"
      },
      "timeoutSecs": 30,
      "enabled": true
    }
  }
}
```

### Streamable HTTP

```json
{
  "mcpServers": {
    "remote-docs": {
      "transport": "streamable-http",
      "url": "https://example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${MCP_TOKEN}"
      },
      "auth": {
        "clientId": "${MCP_CLIENT_ID}",
        "scopes": ["mcp:read"]
      },
      "enabled": true
    }
  }
}
```

规则：

- `transport` 支持 `stdio` 和 `streamable-http`；省略时按 `command` 或 `url` 推断。
- `cwd` 若设置，必须位于当前 Session 的 `working_directory` 内。
- `headers`、OAuth client 字段和 stdio `env` 支持 `${ENV_NAME}`。
- OAuth token 存在本机 `~/.lingclaw/mcp-auth.json`，过期时尝试 refresh。
- Session policy 可以限制 server、具体 tool、mutating tool 确认和是否暴露 workspace root。
- Sub-agent 只继承当前 Session 已启用且通过自身工具 policy 的 MCP tools。
- `lingclaw mcp-check` 做完整诊断；`/mcp refresh` 清除当前 workspace 的缓存、空闲会话与失败冷却后重新发现。

## S3-compatible 图片存储

```json
{
  "settings": {
    "enableS3": true
  },
  "s3": {
    "endpoint": "https://s3.us-east-1.amazonaws.com",
    "region": "us-east-1",
    "bucket": "my-bucket",
    "accessKey": "your-access-key",
    "secretKey": "your-secret-key",
    "prefix": "lingclaw/images/",
    "urlExpirySecs": 604800,
    "lifecycleDays": 14
  }
}
```

S3 用于用户上传和工具图片闭环：

- 只持久化 object key、配置身份、名称和 MIME；不会保存签名 URL 或原始 Base64。
- 配置身份变化后，旧附件不会被错误地映射到新 bucket。
- OpenAI/Anthropic 直接消费签名 URL，因此 endpoint 必须能被远端 Provider 访问。
- Gemini/Ollama 由 LingClaw 本地获取对象并转换为 `inlineData`/Base64，可使用私网 endpoint。
- 每条用户消息和每个工具批次最多 10 张图片，单张上限 10MB，仅接受内容验证通过的 PNG/JPEG。
- `urlExpirySecs` 控制签名 URL 的有效期。
- `lifecycleDays` 默认为 `14`。当其大于 `0` 时，LingClaw 会在启动及相关 Settings 保存后读取 bucket 生命周期配置，并合并或更新当前 `prefix` 的过期规则；设为 `0` 可关闭这项同步。

建议为 LingClaw 使用独立 bucket 或 prefix，并授予所需的最小权限。除对象读写权限外，启用生命周期同步还需要 `s3:GetLifecycleConfiguration` 和 `s3:PutLifecycleConfiguration`。同步失败只会记录警告，不会阻止 LingClaw 启动。

S3 的 `accessKey` 和 `secretKey` 当前按 JSON 字面值读取，不支持 `${ENV_NAME}` 占位符。请限制配置文件权限，避免把真实凭据提交到版本库。

## 环境变量

大多数运行参数、Agent 角色模型和旧式 API Base/Key 的优先级是：JSON 配置 → 对应环境变量 → 内置默认值。`LINGCLAW_PROVIDER` 和 `LINGCLAW_ENABLE_S3` 是例外：只要值有效，它们会覆盖 JSON。Provider 的 `baseUrl`、`apiKey` 以及 MCP 的受支持字段可以使用完整的 `${ENV_NAME}` 占位符；这属于读取 JSON 时的变量展开，不表示同名环境变量总能覆盖显式 JSON 值。

| 变量 | 说明 |
|---|---|
| `OPENAI_API_KEY` | OpenAI/OpenAI-compatible 默认 Key |
| `ANTHROPIC_API_KEY` | Anthropic Key |
| `OLLAMA_API_KEY` | Ollama Key，本地实例通常为空 |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | Gemini Key |
| `OPENAI_API_BASE` | OpenAI-compatible Base URL |
| `OLLAMA_API_BASE` | Ollama Base URL |
| `GEMINI_API_BASE` | Gemini Base URL |
| `LINGCLAW_PROVIDER` | 旧式单 Provider 强制选择 |
| `LINGCLAW_MODEL` | 显式 Primary model |
| `LINGCLAW_FAST_MODEL` | Fast model |
| `LINGCLAW_SUB_AGENT_MODEL` | 默认 Sub-agent model |
| `LINGCLAW_MEMORY_MODEL` | Structured Memory model |
| `LINGCLAW_REFLECTION_MODEL` | Daily Reflection model |
| `LINGCLAW_CONTEXT_MODEL` | Context compression model |
| `LINGCLAW_PORT` | 服务端口 |
| `LINGCLAW_EXEC_TIMEOUT` | `exec` 秒级超时 |
| `LINGCLAW_TOOL_TIMEOUT` | 工具秒级超时 |
| `LINGCLAW_SUB_AGENT_TIMEOUT` | Sub-agent 总超时 |
| `LINGCLAW_MAX_LLM_RETRIES` | LLM 瞬态错误重试次数 |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | 默认上下文预算 |
| `LINGCLAW_OPENAI_STREAM_INCLUDE_USAGE` | OpenAI stream usage 开关 |
| `LINGCLAW_ANTHROPIC_PROMPT_CACHING` | Anthropic prompt caching 开关 |
| `LINGCLAW_STRUCTURED_MEMORY` | Structured Memory 开关 |
| `LINGCLAW_DAILY_REFLECTION` | Daily Reflection 开关 |
| `LINGCLAW_ENABLE_S3` | S3 总开关，优先于 JSON |

布尔值接受 `1/0`、`true/false`、`yes/no` 和 `on/off`。Provider 字段中的 `${ENV_NAME}` 只支持整个字符串精确占位，不做字符串片段插值。

## 校验、备份与故障排查

- Settings 保存前验证 Provider 名称、协议、模型 ID、Agent 引用、MCP transport/cwd 和 S3 必填字段。
- 配置通过临时文件和 rename 原子保存；重新运行 Setup Wizard 会备份已有配置。
- JSON 语法错误时 Settings 进入修复状态，不会用默认值覆盖原文件。
- Provider 测试失败不会自动保存或替换模型。
- 某个 MCP server 启动失败不会阻止 LingClaw 服务启动；状态可在 Settings、`/mcp` 或 `lingclaw mcp-check` 查看。
- 使用 `lingclaw doctor` 检查 Rust、Cargo、Git、Node/npm、源码与已安装版本。

`.lingclaw.json` 和 `mcp-auth.json` 可能包含明文凭据。限制文件权限，不要提交到 Git，也不要把真实值放进截图或问题报告。
