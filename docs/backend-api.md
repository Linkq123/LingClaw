# LingClaw 后端接口文档

本文档基于当前代码实现整理，覆盖 LingClaw 后端的 HTTP API、WebSocket 协议、鉴权约束、配置结构与常见错误语义。

适用范围：

- 后端入口：[src/main.rs](../src/main.rs)
- WebSocket 输入处理：[src/runtime_loop/socket_input.rs](../src/runtime_loop/socket_input.rs)
- 会话同步与历史回放：[src/socket_sync.rs](../src/socket_sync.rs)、[src/session_store.rs](../src/session_store.rs)
- 配置结构：[src/config.rs](../src/config.rs)
- 图片上传：[src/image_uploads.rs](../src/image_uploads.rs)
- 子代理与编排事件：[src/subagents/executor.rs](../src/subagents/executor.rs)、[src/subagents/orchestrator.rs](../src/subagents/orchestrator.rs)

## 1. 总览

- 默认监听地址：`127.0.0.1:18989`
- HTTP 基础地址：`http://127.0.0.1:18989`
- WebSocket 地址：`ws://127.0.0.1:18989/ws`
- 服务框架：`axum`
- 会话模型：默认会话为 `main`，同时支持多个持久化 session

后端暴露两类接口：

- HTTP：健康检查、session 摘要、session 系统 Skills 开关、配置读写、todos、模型与 MCP 联通性测试、Usage、图片上传、优雅关停
- WebSocket：聊天主通道，承载流式回复、工具事件、推理事件、子代理事件、编排事件

## 2. 访问与鉴权约束

### 2.1 本地访问限制

所有 `/api/*` 路由和 `/ws` 都挂了本地请求校验中间件：

- `Host` 必须是 `localhost` 或 loopback 地址
- 如果存在 `Origin` / `Referer`，其 URL host 也必须是 `localhost` 或 loopback 地址

不满足时通常返回：

```json
{
  "error": "Blocked non-local request: Host header must target localhost or a loopback address"
}
```

或：

```json
{
  "error": "Blocked non-local request: Origin/Referer must be localhost or a loopback address"
}
```

状态码：`403 Forbidden`

### 2.2 额外鉴权

除本地限制外，还有两类接口需要额外 token：

1. 图片上传 `POST /api/upload-images`
   - 请求头：`X-LingClaw-Upload-Token`
   - token 通过 `GET /api/client-config` 获取

2. 优雅关停 `POST /api/shutdown`
   - 请求头：`Authorization: Bearer <shutdown-token>`
   - 该 token 由本地 CLI 使用

## 3. 通用约定

### 3.1 内容类型

- 大多数 HTTP 接口使用 `application/json`
- 图片上传使用 `multipart/form-data`
- WebSocket 文本帧承载字符串或 JSON 字符串

### 3.2 错误风格

后端错误返回并不完全统一，主要有三种模式：

1. 标准 HTTP 错误
   - 例如 `400/403/401/500`
   - body 形如 `{ "error": "..." }`

2. `200 OK` + 业务失败
   - 例如 `/api/config/test-model`
   - body 形如 `{ "ok": false, "error": "..." }`

3. 配置文件语法错误但仍返回 `200 OK`
   - 例如 `GET /api/config`
   - body 中带 `parse_error`、`raw`、`line`、`column`

### 3.3 会话范围

服务端默认会话为 `main`，同时支持多个持久化 session。`/api/sessions` 会返回当前已加载或已持久化的 session 摘要；WebSocket 连接可通过查询参数 `?session=<id>` 绑定到指定 session，省略时回退到 `main`。每个 session 还持有一份当前 `todos` 快照（`revision`、`items[]`、`last_updated_by`、`updated_at`），随会话一起持久化；执行 `/clear` 时会清空 `items[]` 并推进 `revision`，从而拒绝旧的 in-flight 写入。

## 4. HTTP API

## 4.1 GET /api/health

健康检查。

### 响应

```json
{
  "status": "ok",
  "version": "2.x.x",
  "model": "openai/gpt-4o-mini",
  "sessions": 1
}
```

### 字段说明

- `status`：固定为 `ok`
- `version`：后端版本
- `model`：当前默认模型
- `sessions`：当前内存中会话数量

## 4.2 GET /api/sessions

返回当前已知 session 摘要列表。

### 响应

```json
{
  "sessions": [
    {
      "id": "main",
      "name": "Main",
      "messages": 42,
      "tool_calls": 18,
      "model": "openai/gpt-4o-mini",
      "created_at": 1710000000,
      "updated_at": 1710001234
    },
    {
      "id": "research-notes",
      "name": "research-notes",
      "messages": 7,
      "tool_calls": 2,
      "model": "openai/gpt-4o-mini",
      "created_at": 1710002222,
      "updated_at": 1710003333
    }
  ]
}
```

### 说明

- `main` 固定排在第一；其他 session 按 `updated_at` 倒序排列
- `messages` 为会话消息条数
- `tool_calls` 为累计工具调用次数
- 列表同时覆盖默认 `main` 和其他已创建 session

## 4.2.1 POST /api/session

创建一个新的持久化 session。session id 由后端随机生成，格式为 6 位小写英文字母或数字；用户不需要输入 id，可在创建后通过重命名修改显示名称。

### 请求

无请求体。

### 响应

```json
{
  "ok": true,
  "session": {
    "id": "a1b2c3",
    "name": "Session a1b2c3",
    "messages": 0,
    "tool_calls": 0,
    "model": "openai/gpt-4o-mini",
    "created_at": 1710002222,
    "updated_at": 1710002222,
    "corrupt": false
  }
}
```

### 说明

- 新 session 会立即写入 `~/.lingclaw/sessions/<id>.json`
- 创建成功后会广播新的 session 列表
- 若随机 id 碰撞，后端会重新生成；连续失败返回 `500`

## 4.2.2 PUT /api/session

修改指定 session 的显示名称；session id 和工作区路径不变。

### 查询参数

- `session`：可选 session id，省略时使用 `main`

### 请求

```json
{
  "name": "Research Notes"
}
```

### 响应

```json
{
  "ok": true,
  "session": {
    "id": "research-notes",
    "name": "Research Notes",
    "messages": 7,
    "tool_calls": 2,
    "model": "openai/gpt-4o-mini",
    "created_at": 1710002222,
    "updated_at": 1710004444,
    "corrupt": false
  }
}
```

### 说明

- `name` 会去除首尾空白，不能为空，最长 80 个字符
- 保存成功后会持久化 session，并广播新的 session 列表
- 未知 session 返回 `404`，非法名称返回 `400`

## 4.3 GET/PUT /api/session-skills

管理指定 session 可注入的系统内置 Skills。系统 Skills 默认不注入，只有在该 session 中启用后才进入系统提示。该接口只覆盖 `system` 来源的 Skills；`global` 和 `session` 来源仍按目录自动发现并注入，不在 Settings 页面中启停。

### 查询参数

- `session`：可选 session id，省略时使用 `main`

### GET 响应

```json
{
  "session": {
    "id": "main",
    "name": "Main"
  },
  "skills": [
    {
      "id": "anthropics/pdf",
      "name": "pdf",
      "description": "PDF processing workflow",
      "path": "system://skills/anthropics/pdf/SKILL.md",
      "group": "anthropics",
      "enabled": false
    }
  ],
  "enabledSystemSkills": [],
  "disabledSystemSkills": ["anthropics/pdf"]
}
```

### PUT 请求体

```json
{
  "enabledSystemSkills": ["anthropics/pdf", "anthropics/xlsx"],
  "knownSystemSkills": ["anthropics/pdf", "anthropics/xlsx", "anthropics/pptx"]
}
```

### PUT 成功响应

```json
{
  "ok": true,
  "session": {
    "id": "main",
    "name": "Main"
  },
  "skills": [],
  "enabledSystemSkills": ["anthropics/pdf", "anthropics/xlsx"],
  "disabledSystemSkills": ["anthropics/pptx"]
}
```

### 说明

- `skill.id` 是系统 Skill 相对目录，如 `anthropics/pdf`
- 新建和迁移后的旧 session 默认所有系统 Skills 关闭
- PUT 会把 `enabledSystemSkills` 保存为 session 的 `enabled_system_skills`
- `knownSystemSkills` 可选；提供后，后端只更新这批客户端已加载的 Skills，未包含其中但服务端后来新发现的 Skills 会保留原状态，避免 Settings 页面保存时误开启或误关闭新增 Skills
- 保存后会刷新该 session 的 system prompt；只有启用的系统 Skill 会出现在 `## Skills`
- 未知 session 返回 `404`，未知 Skill id 返回 `400`

## 4.4 GET /api/client-config

返回前端运行所需的轻量配置。目前主要用于图片上传 token。

### 响应

```json
{
  "upload_token": "..."
}
```

### 说明

- 仅本地请求可访问
- `upload_token` 由前端拿到后用于 `POST /api/upload-images`

## 4.5 GET /api/config

读取原始配置文件 `~/.lingclaw/.lingclaw.json`，并附带已发现的子代理摘要。

### 成功响应

```json
{
  "config": {
    "settings": {},
    "models": {},
    "agents": {},
    "mcpServers": {},
    "s3": {}
  },
  "path": "C:\\Users\\admin\\.lingclaw\\.lingclaw.json",
  "discoveredAgents": [
    {
      "name": "reviewer",
      "description": "Code review specialist",
      "source": "system"
    }
  ]
}
```

### 配置语法错误响应

```json
{
  "config": null,
  "raw": "{ ...损坏的原始 JSON... }",
  "path": "C:\\Users\\admin\\.lingclaw\\.lingclaw.json",
  "parse_error": "expected `:` at line 12 column 9",
  "line": 12,
  "column": 9,
  "discoveredAgents": []
}
```

### 字段说明

- `config`：解析成功时返回对象，失败时为 `null`
- `raw`：仅在解析失败时返回原始文本
- `path`：配置文件绝对路径
- `parse_error`：`serde_json` 错误文本
- `line` / `column`：尽力提取出的语法错误位置
- `discoveredAgents`：当前 workspace 发现到的子代理

### discoveredAgents 结构

```json
{
  "name": "reviewer",
  "description": "Code review specialist",
  "source": "system"
}
```

## 4.6 PUT /api/config

校验并保存配置文件。保存成功后会：

- 原子写入配置文件
- 热重载运行时配置
- 刷新前端会话能力信息
- 刷新 MCP server 缓存

### 请求体

顶层必须包含 `config` 字段：

```json
{
  "config": {
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
      "enableStateDigest": true,
      "enableS3": true,
      "openaiStreamIncludeUsage": false,
      "anthropicPromptCaching": false
    },
    "models": {
      "providers": {
        "openai": {
          "api": "openai-completions",
          "baseUrl": "https://api.openai.com/v1",
          "apiKey": "sk-...",
          "models": [
            {
              "id": "gpt-4o-mini",
              "name": "gpt-4o-mini",
              "reasoning": false,
              "input": ["text", "image"],
              "contextWindow": 128000,
              "maxTokens": 16384,
              "cost": {},
              "compat": {
                "thinkingFormat": "openai"
              }
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
          "context": "openai/gpt-4o-mini",
          "sub-agent-reviewer": "openai/gpt-4o-mini"
        }
      }
    },
    "mcpServers": {
      "filesystem": {
        "command": "uvx",
        "args": ["mcp-server-filesystem"],
        "env": {
          "DEBUG": "1"
        },
        "cwd": ".",
        "enabled": true,
        "timeoutSecs": 30
      }
    },
    "s3": {
      "endpoint": "https://s3.us-east-1.amazonaws.com",
      "region": "us-east-1",
      "bucket": "my-bucket",
      "accessKey": "AKIA...",
      "secretKey": "secret",
      "prefix": "lingclaw/images/",
      "urlExpirySecs": 604800,
      "lifecycleDays": 14
    }
  }
}
```

### 成功响应

```json
{
  "ok": true
}
```

### 校验规则摘要

#### settings

- `port`: `u16`
- `execTimeout`, `toolTimeout`, `subAgentTimeout`: 秒
- `subAgentTimeout = 0` 表示不限时
- `maxLlmRetries`: 非负整数
- `enableStateDigest` 默认可开启

#### models.providers

每个 provider 项结构：

```json
{
  "api": "openai-completions | openai-responses | anthropic | ollama | gemini",
  "baseUrl": "string",
  "apiKey": "string",
  "models": [
    {
      "id": "string",
      "name": "string?",
      "reasoning": true,
      "input": ["text", "image"],
      "contextWindow": 128000,
      "maxTokens": 8192,
      "cost": {},
      "compat": {
        "thinkingFormat": "string?"
      }
    }
  ]
}
```

约束：

- provider 名称不能为空
- provider 名称不能包含 `/`
- provider 名称不能包含空白字符
- provider 名称只允许字母、数字、`.`、`-`、`_`
- `api` 只允许：
  - `openai-completions`
  - `openai-responses`
  - `anthropic`
  - `ollama`
  - `gemini`
- `openai-completions` 对应 `POST /v1/chat/completions`
- `openai-responses` 对应 `POST /v1/responses`
- 对话路径下，`openai-responses` 会设置 `stream: true` 并消费 Responses SSE 事件，把 `output_text`、reasoning summary、`function_call` 参数增量和最终 `response.completed` 映射回 LingClaw 现有消息结构与前端 WebSocket 事件流
- `baseUrl` 不能为空
- `baseUrl` / `apiKey` 可以直接写字面值，也可以写成精确的 `${ENV_NAME}` 占位符；运行时会按环境变量展开
- `models[].id` 不能为空
- `models[].compat` 如提供，必须是对象
- `models[].compat.thinkingFormat` 如提供，必须是字符串；用于显式声明 OpenAI-compatible 的 thinking / reasoning 方言（例如 `openai`、`qwen`、`doubao`、`deepseek-v4`、`ollama`、`gpt-oss`）
- `models[].compat.reasoning.summary` 如提供，必须是字符串；仅 `openai-responses` 使用，会透传到 Responses API 的 `reasoning.summary`
- `models[].compat.thinkingFormat = "deepseek-v4"` 时，请求会显式发送 `thinking.type=enabled|disabled`；开启 thinking 时，`reasoning_effort` 仅使用 `high` / `max`
- `models[].compat.thinkingFormat = "doubao"` 时，请求会显式发送 `thinking.type=enabled|disabled`；开启 thinking 时，`reasoning_effort` 仅使用 `low` / `medium` / `high`

#### agents.defaults.model

支持字段：

- `primary`
- `fast`
- `sub-agent`
- `memory`
- `reflection`
- `context`
- 任意 `sub-agent-<name>` 动态覆盖项

约束：

- 如果写成 `provider/model-id` 形式，则 provider 必须存在于 `models.providers`
- 如果该 provider 已定义 `models` 列表，则 `model-id` 必须存在
- 当 `models.providers` 为空时，允许使用内置 provider 前缀：
  - `openai`
  - `anthropic`
  - `ollama`
  - `gemini`

#### mcpServers

每个 MCP server 项结构：

```json
{
  "command": "string",
  "args": ["string"],
  "env": {
    "KEY": "VALUE"
  },
  "cwd": ".",
  "enabled": true,
  "timeoutSecs": 30
}
```

约束：

- `command` 不能为空
- `timeoutSecs` 不能为 `0`
- `cwd` 必须位于当前配置测试所使用 session 的 workspace 内
- `cwd` 不允许逃逸 workspace
- `cwd` 不允许穿过受保护 symlink
- `cwd` 不允许指向 `.lingclaw-bootstrap`

#### s3

启用本地图片上传时使用，字段：

- `endpoint`
- `region`
- `bucket`
- `accessKey`
- `secretKey`
- `prefix`
- `urlExpirySecs`
- `lifecycleDays`

### 典型错误响应

#### 缺少 `config`

状态码：`400`

```json
{
  "error": "Missing 'config' field"
}
```

#### `config` 不是对象

状态码：`400`

```json
{
  "error": "Config must be a JSON object"
}
```

#### provider 名称非法

状态码：`400`

```json
{
  "error": "Invalid models.providers entry 'openai/test': Provider name cannot contain '/'."
}
```

#### agent 默认模型引用非法

状态码：`400`

```json
{
  "error": "Invalid agents.defaults.model.primary: unknown provider 'missing'. Add it in models.providers first."
}
```

## 4.7 POST /api/config/test-model

测试模型 provider 连通性。后端会用给定配置发一个最小请求，消息内容固定为 `"Hi"`。

### 请求体

```json
{
  "providerName": "openai",
  "baseUrl": "https://api.openai.com/v1",
  "apiKey": "sk-...",
  "api": "openai-completions",
  "modelId": "gpt-4o-mini"
}
```

### 字段说明

- `providerName`: 可选；当 `baseUrl` / `apiKey` 使用 `${ENV_NAME}` 占位符时，后端只会在该 provider 已保存且请求值与已保存配置完全一致时，使用当前运行配置进行测试
- `baseUrl`: 必填
- `apiKey`: 可为空，是否必需由 provider 决定
- `baseUrl` / `apiKey`: 在配置文件中也可以写成 `${ENV_NAME}`，例如 `${OPENAI_API_BASE}` / `${OPENAI_API_KEY}`
- `api`: 默认 `openai-completions`，也可显式传 `openai-responses`
- `modelId`: 必填

### 成功响应

```json
{
  "ok": true,
  "reply": "Hello ..."
}
```

### 业务失败响应

状态码通常仍为 `200`

```json
{
  "ok": false,
  "error": "..."
}
```

### 参数错误响应

状态码：`400`

```json
{
  "error": "baseUrl and modelId are required"
}
```

## 4.8 POST /api/config/test-mcp

测试 MCP server 是否可启动并完成 tools 列表发现。

### 请求体

```json
{
  "command": "uvx",
  "args": ["mcp-server-filesystem"],
  "env": {
    "DEBUG": "1"
  },
  "cwd": ".",
  "timeoutSecs": 30
}
```

### 成功响应

```json
{
  "ok": true,
  "tools": 12
}
```

### 业务失败响应

```json
{
  "ok": false,
  "error": "..."
}
```

### 参数错误响应

状态码：`400`

```json
{
  "error": "command is required"
}
```

### 超时响应

状态码通常仍为 `200`

```json
{
  "ok": false,
  "error": "Connection timed out"
}
```

## 4.9 GET /api/usage

返回当前默认 session（`main`）的 token 统计。

### 响应

```json
{
  "daily_input": 1200,
  "daily_output": 340,
  "total_input": 5000,
  "total_output": 1800,
  "total": 6800,
  "input_source": "provider",
  "output_source": "estimated",
  "source_scope": "latest_update",
  "usage_history": [
    {
      "date": "2026-05-02",
      "input": 1000,
      "output": 200,
      "providers": {
        "openai": [1000, 200]
      },
      "roles": {
        "primary": [800, 150],
        "sub-agent": [200, 50]
      }
    }
  ],
  "daily_providers": {
    "openai": [1200, 340]
  },
  "daily_roles": {
    "primary": [900, 250],
    "sub-agent": [300, 90]
  },
  "total_providers": {
    "openai": [5000, 1800]
  },
  "total_roles": {
    "primary": [4200, 1500],
    "sub-agent": [800, 300]
  }
}
```

### 字段说明

- `daily_*`: 当日统计
- `total_*`: 当前会话累计统计
- `input_source` / `output_source`:
  - 常见值：`provider`、`estimated`
- `source_scope`: 当前固定为 `latest_update`
- `providers` / `roles`: 值格式均为 `[input_tokens, output_tokens]`

## 4.10 PUT /api/todos

原子替换指定 session 的当前 todos 清单。

- 查询参数：`session=<id>`（可选，省略时默认 `main`）
- 请求体采用“整表替换 + revision 乐观并发”协议
- 成功时返回最新快照
- 若 `base_revision` 已过期，则返回 `409 Conflict` 和当前服务端快照，不落盘、不覆盖新数据

### 请求体

```json
{
  "base_revision": 3,
  "items": [
    {
      "id": "todo-1",
      "content": "Review runtime loop changes",
      "status": "in_progress"
    },
    {
      "id": "todo-2",
      "content": "Update backend API docs",
      "status": "pending"
    }
  ]
}
```

### 请求规则

- `items` 表示完整有序列表，服务端不会做局部 merge
- 允许空数组，表示清空 todos
- 最多 `12` 项
- `id` 必须唯一、非空，最长 `64` 字符
- `content` 必须非空，最长 `200` 字符
- `status` 仅允许：`pending`、`in_progress`、`completed`
- 整个列表最多只允许 `1` 个 `in_progress`

### 成功响应

状态码：`200 OK`

```json
{
  "ok": true,
  "conflict": false,
  "revision": 4,
  "items": [
    {
      "id": "todo-1",
      "content": "Review runtime loop changes",
      "status": "in_progress"
    },
    {
      "id": "todo-2",
      "content": "Update backend API docs",
      "status": "pending"
    }
  ],
  "last_updated_by": "user",
  "updated_at": 1710002345
}
```

字段说明：

- `ok`：本次写入是否生效
- `conflict`：是否发生 revision 冲突
- `revision`：服务端最新 revision
- `items`：服务端权威有序列表
- `last_updated_by`：最近一次成功写入来源，`user` 或 `assistant`
- `updated_at`：最新快照时间戳（Unix 秒）

### 冲突响应

状态码：`409 Conflict`

```json
{
  "ok": false,
  "conflict": true,
  "revision": 5,
  "items": [
    {
      "id": "todo-1",
      "content": "Review runtime loop changes",
      "status": "completed"
    }
  ],
  "last_updated_by": "assistant",
  "updated_at": 1710002400
}
```

说明：

- 该响应里的 `items` / `revision` 就是当前服务端权威快照
- 客户端应以它覆盖本地临时状态，再基于新 `revision` 重试

### 参数或校验错误

状态码：`400 Bad Request`

```json
{
  "error": "todos error: only one item may use status 'in_progress'"
}
```

### 典型错误状态码

- `400 Bad Request`：session id 非法、JSON 不合法或 todos 校验失败
- `404 Not Found`：指定 session 不存在且无法加载
- `409 Conflict`：`base_revision` 落后
- `500 Internal Server Error`：持久化失败

## 4.11 POST /api/upload-images

上传本地图片到 S3-compatible 存储，并返回可用 URL 与受信 object key。

### 请求头

- `X-LingClaw-Upload-Token: <token>`

### 请求体

`multipart/form-data`

- 字段名：前端当前使用 `file`
- 可包含多个同名文件字段

### 上传限制

- 最多 `10` 张图
- 单张最大 `10 MB`
- 整个请求体上限约 `101 MB`
- 仅支持 `JPEG`、`PNG`
- 以后端内容检测结果为准，不信任浏览器声明的 MIME

### 成功响应

```json
{
  "images": [
    {
      "url": "https://...presigned...",
      "object_key": "lingclaw/images/2026-05-02/....png",
      "attachment_token": "..."
    }
  ],
  "urls": [
    "https://...presigned..."
  ],
  "errors": []
}
```

### 字段说明

- `images`: 推荐前端保存，包含可信上传元信息
- `urls`: 仅 URL 列表，便于兼容旧逻辑
- `errors`: 局部失败列表；即使某些文件失败，其他文件仍可成功

### 典型错误

#### 缺少 upload token

状态码：`403`

```json
{
  "error": "Missing upload token"
}
```

#### upload token 无效

状态码：`403`

```json
{
  "error": "Invalid upload token"
}
```

#### S3 未配置

状态码：`400`

```json
{
  "error": "S3 not configured"
}
```

#### 文件级错误样例

```json
{
  "images": [],
  "urls": [],
  "errors": [
    "Maximum 10 images per upload",
    "Empty image file",
    "Unsupported image content (declared type: image/webp)",
    "Image too large (12345678 bytes, max 10485760)",
    "S3 upload timed out"
  ]
}
```

## 4.12 POST /api/shutdown

供本地 CLI 调用的优雅关停接口。

### 请求头

```http
Authorization: Bearer <shutdown-token>
```

### 成功响应

```json
{
  "status": "shutting_down"
}
```

### 鉴权失败

状态码：`401`

```json
{
  "error": "unauthorized"
}
```

## 5. WebSocket 协议

## 5.1 连接地址

```text
ws://127.0.0.1:18989/ws
```

也可以通过查询参数绑定到指定 session：

```text
ws://127.0.0.1:18989/ws?session=research-notes
```

- `session` 省略时默认绑定 `main`
- 指定的 session 不存在时，服务端会按该 id 创建新 session（前提是 id 合法）
- 非法 session id 会回退到 `main`，并额外推送一条 `error` 事件说明原因

建立连接后，服务端通常会按以下顺序推送初始化事件：

1. `session`
2. `view_state`
3. `todos_state`
4. `history`

## 5.2 客户端 -> 服务端

客户端当前支持 4 类输入。

### 5.2.1 纯文本消息

直接发送字符串：

```text
帮我检查这个仓库的配置问题
```

服务端会将其作为当前 WebSocket 绑定 session 的用户消息，然后启动一轮 agent 执行。

### 5.2.2 Slash 命令

直接发送字符串命令：

```text
/help
/tool off
/reasoning on
/stop
```

说明：

- 空闲时可执行命令
- 忙碌时仅允许一小部分运行期控制命令，尤其是 `/stop`

### 5.2.3 图片消息 JSON

当携带图片时，发送 JSON 字符串：

```json
{
  "text": "请分析这张图",
  "images": [
    {
      "url": "https://...",
      "object_key": "lingclaw/images/2026-05-02/....png",
      "attachment_token": "..."
    }
  ]
}
```

`images[]` 元素结构：

```json
{
  "url": "https://...",
  "object_key": "optional",
  "attachment_token": "optional"
}
```

说明：

- `object_key + attachment_token` 成对使用
- 若两者都存在，服务端会把该图当作受信任的已上传对象
- 若只传 `url`，服务端会按普通远程图片 URL 校验
- 最多 `10` 张图

### 5.2.4 忙碌期干预

当主 agent 正在运行时：

- 普通文本不会立刻开启新一轮，而是作为 deferred intervention 排队
- 带图干预只保留文本，图片会被丢弃
- `/stop` 会立即请求中止当前执行

## 5.3 服务端 -> 客户端事件

下表列出前端需要处理的主要事件。

## 5.3.1 会话与历史

### `session`

```json
{
  "type": "session",
  "id": "main",
  "name": "Main",
  "capabilities": {
    "image": true,
    "s3": true
  },
  "usage": {
    "daily_input": 100,
    "daily_output": 20,
    "total_input": 500,
    "total_output": 100
  }
}
```

字段说明：

- `capabilities.image`: 当前有效模型是否支持图片输入
- `capabilities.s3`: 当前服务端是否可用 S3 上传能力

### `view_state`

```json
{
  "type": "view_state",
  "show_tools": true,
  "show_reasoning": true,
  "show_react": true
}
```

### `todos_state`

```json
{
  "type": "todos_state",
  "revision": 4,
  "items": [
    {
      "id": "todo-1",
      "content": "Review runtime loop changes",
      "status": "in_progress"
    },
    {
      "id": "todo-2",
      "content": "Update backend API docs",
      "status": "pending"
    }
  ],
  "last_updated_by": "assistant",
  "updated_at": 1710002400
}
```

说明：

- 这是会话级 todo 面板的唯一权威数据源
- 首次连接、切换 session、重连回放、用户编辑、主代理调用 `todos` 工具后，都会重新发送
- `items` 顺序即 UI 展示顺序
- `last_updated_by = user` 时，表示最近一次成功写入来自前端 `/api/todos`

### `history`

```json
{
  "type": "history",
  "messages": [
    {
      "role": "user",
      "content": "你好",
      "timestamp": 1710000000,
      "images": [
        {
          "url": "https://..."
        }
      ]
    },
    {
      "role": "assistant",
      "content": "你好，我在。",
      "timestamp": 1710000001,
      "thinking": "..."
    },
    {
      "role": "tool_call",
      "name": "read_file",
      "arguments": "{\"path\":\"README.md\"}",
      "id": "call_123"
    },
    {
      "role": "tool_result",
      "result": "file content ...",
      "id": "call_123",
      "is_error": false,
      "subagent_snapshot": {
        "cycles": 3,
        "tool_calls": 2,
        "duration_ms": 2400,
        "input_tokens": 120,
        "output_tokens": 80,
        "success": true,
        "result_excerpt": "..."
      }
    }
  ]
}
```

`history.messages[].role` 当前可见值：

- `user`
- `assistant`
- `tool_call`
- `tool_result`

补充说明：

- `todos` 工具的 `tool_call` / `tool_result` 不会进入这里的可见历史列表
- 前端应使用 `todos_state` 渲染 todo 面板，而不是从 `history.messages` 反推

## 5.3.2 一轮主执行中的基础事件

### `start`

表示一轮回复开始。

```json
{
  "type": "start",
  "round": 3,
  "phase": "analyze",
  "cycle": 1,
  "model": "openai/gpt-4o-reasoner",
  "think_level": "high",
  "react_visible": true,
  "auto_observation_strength": "medium",
  "auto_stagnation_streak": 1,
  "auto_error_streak": 0,
  "auto_task_pressure": 2,
  "auto_action_oriented": true,
  "auto_ready_to_finish": false,
  "auto_has_blocking_uncertainty": true,
}
```

补充说明：

- `model` / `think_level` 表示本轮实际使用的模型与思维级别；它们可能与静态配置不同，例如被运行时路由或 Hook 覆盖
- `phase` / `cycle` 为当前顶层主代理的 live runtime 状态
- 以 `auto_*` 开头的字段仅在 `/think auto` 且当前模型支持 reasoning effort 时出现，用于给 `/status` 与重连回放提供实时摘要

### `auto_trace`

`think=auto` 的顶层决策轨迹。该事件只针对主代理当前 round 发送；子代理即使内部也使用 auto 策略，其轨迹也不会污染顶层面板或主会话 live state。

```json
{
  "type": "auto_trace",
  "round": 3,
  "cycle": 1,
  "phase": "analyze",
  "model": "openai/gpt-4o-reasoner",
  "provider": "openai",
  "selected_think": "high",
  "baseline_level": "medium",
  "baseline_reason": "action_oriented_first_turn",
  "escalators": ["blocking_uncertainty"],
  "dampeners": [],
  "clamps": [],
  "signals": {
    "intent": "change",
    "user_msg_chars": 148,
    "observation_strength": "medium",
    "tool_results_count": 2,
    "tool_error_count": 0,
    "summary_count": 1,
    "summary_bytes": 1024,
    "stagnation_streak": 1,
    "error_streak": 0,
    "task_pressure": 2,
    "ready_to_finish": false,
    "action_oriented": true,
    "has_blocking_uncertainty": true,
    "progress_made": true,
    "retry_pattern": "same_tool",
    "error_kind": "none",
    "evidence_delta_quality": "better_evidence"
  }
}
```

补充说明：

- `selected_think` 为最终发送给模型的思维级别；若 `BeforeLlmCall` Hook 覆盖了 think，trace 会直接反映覆盖后的值，并在 `clamps` 中加入 `hook_think_override`
- `baseline_*` 描述本轮 runtime auto policy 在未叠加 escalator / dampener / clamp 之前的基线判断
- `signals` 是用于 auto-think 决策的实时输入快照，也是 `/status` 中 `auto_signals` / `auto_decision` 摘要的来源；其中 `ready_to_finish` / `has_blocking_uncertainty` 现在是 advisory signals，不直接决定主循环是否 finish

### `delta`

主回复流式文本增量。

```json
{
  "type": "delta",
  "content": "增量文本"
}
```

### `thinking_start`

开始输出 reasoning。

```json
{
  "type": "thinking_start"
}
```

### `thinking_delta`

reasoning 文本增量。

```json
{
  "type": "thinking_delta",
  "content": "..."
}
```

### `thinking_done`

reasoning 流结束。

```json
{
  "type": "thinking_done"
}
```

### `tool_call`

主代理开始调用工具。

```json
{
  "type": "tool_call",
  "name": "read_file",
  "arguments": "{\"path\":\"README.md\"}",
  "id": "call_123"
}
```

### `tool_progress`

长时间运行工具的心跳进度。

```json
{
  "type": "tool_progress",
  "id": "call_123",
  "name": "exec",
  "elapsed_ms": 2300
}
```

### `tool_result`

工具执行完成。

```json
{
  "type": "tool_result",
  "id": "call_123",
  "name": "read_file",
  "result": "....",
  "duration_ms": 120,
  "is_error": false
}
```

当该工具来自子代理时，事件还可能带：

```json
{
  "task_id": "task-1",
  "subagent": "reviewer"
}
```

补充说明：

- 内置 `todos` 工具不会发送普通 `tool_call` / `tool_result` 可视化事件
- 对 todos 的可视化更新统一通过 `todos_state` 推送，避免污染时间线

### `observation`

工具结果摘要，不替代完整 `tool_result`。

```json
{
  "type": "observation",
  "tool_call_id": "call_123",
  "tool_name": "read_file",
  "byte_size": 1024,
  "line_count": 40,
  "hint": "..."
}
```

### `react_phase`

主代理 ReAct 阶段切换。

```json
{
  "type": "react_phase",
  "phase": "analyze",
  "cycle": 2
}
```

`phase` 可见值：

- `analyze`
- `act`
- `observe`
- `finish`

### `done`

一轮主执行结束。

```json
{
  "type": "done",
  "phase": "finish",
  "reason": "complete",
  "cycles": 3,
  "tool_calls": 5,
  "daily_input_tokens": 300,
  "daily_output_tokens": 80,
  "total_input_tokens": 1200,
  "total_output_tokens": 340,
  "round_input_tokens": 200,
  "round_output_tokens": 60
}
```

补充说明：

- 用户主动停止时，可能是：

```json
{
  "type": "done",
  "phase": "stopped",
  "reason": "user_stop"
}
```

## 5.3.3 上下文维护事件

### `context_pruned`

消息窗口裁剪发生。

```json
{
  "type": "context_pruned",
  "messages_removed": 8
}
```

### `context_compressed`

自动上下文压缩成功。

```json
{
  "type": "context_compressed",
  "messages_removed": 20,
  "before_estimate": 28000,
  "after_estimate": 9000,
  "summary_tokens": 700,
  "compression_ratio": 32,
  "incremental": true
}
```

### `context_compress_failed`

自动上下文压缩失败。

```json
{
  "type": "context_compress_failed",
  "error": "..."
}
```

## 5.3.4 子代理任务事件

### `task_started`

主代理通过 `task` 工具发起子代理任务。

```json
{
  "type": "task_started",
  "task_id": "task-1",
  "agent": "reviewer",
  "prompt": "..."
}
```

### `task_progress`

子代理执行进度。

```json
{
  "type": "task_progress",
  "task_id": "task-1",
  "agent": "reviewer",
  "cycle": 1,
  "phase": "analyze"
}
```

### `task_tool`

子代理调用工具。

```json
{
  "type": "task_tool",
  "task_id": "task-1",
  "agent": "reviewer",
  "tool": "read_file",
  "id": "call_123",
  "arguments": "{\"path\":\"src/main.rs\"}"
}
```

### `task_completed`

```json
{
  "type": "task_completed",
  "task_id": "task-1",
  "agent": "reviewer",
  "cycles": 3,
  "tool_calls": 2,
  "input_tokens": 120,
  "output_tokens": 80,
  "duration_ms": 2400,
  "result_preview": "...",
  "result_excerpt": "..."
}
```

### `task_failed`

```json
{
  "type": "task_failed",
  "task_id": "task-1",
  "agent": "reviewer",
  "error": "...",
  "cycles": 2,
  "tool_calls": 1,
  "input_tokens": 60,
  "output_tokens": 20,
  "duration_ms": 900
}
```

## 5.3.5 多子代理编排事件

当主代理使用 `orchestrate` 工具时，会发出一组 DAG 编排事件。

### `orchestrate_started`

```json
{
  "type": "orchestrate_started",
  "orchestrate_id": "abc123",
  "task_count": 3,
  "layer_count": 2,
  "tasks": [
    {
      "id": "explore",
      "agent": "explore",
      "depends_on": [],
      "prompt_preview": "..."
    }
  ]
}
```

### `orchestrate_layer`

```json
{
  "type": "orchestrate_layer",
  "orchestrate_id": "abc123",
  "layer": 1,
  "total_layers": 2,
  "tasks": ["explore", "research"]
}
```

### `orchestrate_task_started`

```json
{
  "type": "orchestrate_task_started",
  "orchestrate_id": "abc123",
  "id": "explore",
  "agent": "explore",
  "prompt": "..."
}
```

### `orchestrate_task_completed`

```json
{
  "type": "orchestrate_task_completed",
  "orchestrate_id": "abc123",
  "id": "explore",
  "agent": "explore",
  "cycles": 2,
  "tool_calls": 3,
  "input_tokens": 100,
  "output_tokens": 50,
  "duration_ms": 1800,
  "result_excerpt": "..."
}
```

### `orchestrate_task_failed`

```json
{
  "type": "orchestrate_task_failed",
  "orchestrate_id": "abc123",
  "id": "explore",
  "agent": "explore",
  "error": "...",
  "cycles": 1,
  "tool_calls": 1,
  "input_tokens": 40,
  "output_tokens": 10,
  "duration_ms": 500
}
```

### `orchestrate_task_skipped`

```json
{
  "type": "orchestrate_task_skipped",
  "orchestrate_id": "abc123",
  "id": "review",
  "agent": "reviewer",
  "reason": "dependency 'explore' failed"
}
```

### `orchestrate_completed`

```json
{
  "type": "orchestrate_completed",
  "orchestrate_id": "abc123",
  "completed": 2,
  "failed": 1,
  "skipped": 0,
  "total_tasks": 3,
  "input_tokens": 260,
  "output_tokens": 90,
  "duration_ms": 4200,
  "aborted": false
}
```

## 5.3.6 通知类事件

### `system`

中性系统提示。

```json
{
  "type": "system",
  "content": "..."
}
```

### `success`

成功提示。

```json
{
  "type": "success",
  "content": "..."
}
```

### `error`

错误提示。

```json
{
  "type": "error",
  "content": "..."
}
```

### `progress`

进度提示。

```json
{
  "type": "progress",
  "content": "..."
}
```

## 6. 图片输入协议细节

### 6.1 受信任上传图片

上传成功后，前端应优先回传：

```json
{
  "url": "https://...",
  "object_key": "lingclaw/images/...",
  "attachment_token": "..."
}
```

这样服务端会：

- 校验 `attachment_token`
- 重新生成可信 URL
- 避免客户端伪造任意 S3 object key

### 6.2 普通远程图片

若仅回传：

```json
{
  "url": "https://example.com/a.png"
}
```

则服务端会将其视为普通远程 URL，并执行图片 URL 安全校验。

### 6.3 服务端拒绝场景

WebSocket 下若图片不合法，通常以 `system` 事件返回错误，例如：

- `Too many images (max 10).`
- `Current model does not support image input.`
- `Invalid uploaded image token. Please re-attach the image.`
- `Incomplete uploaded image metadata. Please re-attach the image.`
- `S3 uploads are no longer configured. Please re-attach the image.`

## 7. 建议的前端接入顺序

如果要从零接入一个客户端，建议顺序如下：

1. 轮询或请求 `GET /api/health`，确认服务可用
2. 建立 `/ws` 连接
3. 收到 `session`、`view_state`、`todos_state`、`history` 后初始化 UI
4. 发送纯文本或图片 JSON 消息
5. 处理 `start -> delta/thinking/tool/* -> done`
6. 如需本地上传图片：
   - 先调用 `GET /api/client-config`
   - 再调用 `POST /api/upload-images`
   - 最后把 `url + object_key + attachment_token` 带回 WebSocket 消息

## 8. 已知实现特征

- `/api/config/test-model` 与 `/api/config/test-mcp` 的“联通性失败”通常返回 `200 + {ok:false}`
- `/api/config` 在配置文件语法错误时不会返回 4xx，而是返回可恢复信息
- `/api/sessions` 返回当前已知 session 摘要列表，`main` 固定置顶；`POST /api/session` 创建随机 6 位 id 的新 session，`PUT /api/session` 只修改 session 显示名称
- `/api/todos` 使用整表替换 + revision 冲突语义；冲突时返回 `409 + 当前快照`
- WebSocket 客户端消息没有显式 `type` 字段，按“纯文本 / slash 命令 / JSON 图片消息”三种形态自动分流
- 忙碌时普通文本会进入 deferred intervention 队列，不会立即中断主执行

## 9. 文档维护建议

后续如果新增接口，建议至少同步更新三处：

1. 本文档
2. `frontend/src/types.ts` 或 `frontend/src/types/config.ts`
3. `src/tests/main_tests.rs` 中对应 API / 事件测试
