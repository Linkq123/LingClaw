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
- 会话模型：当前仅暴露单主会话 `main`

后端暴露两类接口：

- HTTP：健康检查、配置读写、模型与 MCP 联通性测试、Usage、图片上传、优雅关停
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

当前服务端只对外暴露主会话 `main`。`/api/sessions` 也只会返回主会话信息。

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

返回当前主会话摘要。

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
    }
  ]
}
```

### 说明

- 当前实现只返回 `main`
- `messages` 为会话消息条数
- `tool_calls` 为累计工具调用次数

## 4.3 GET /api/client-config

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

## 4.4 GET /api/config

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

## 4.5 PUT /api/config

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
              "compat": {}
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
  "api": "openai-completions | anthropic | ollama | gemini",
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
      "compat": {}
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
  - `anthropic`
  - `ollama`
  - `gemini`
- `baseUrl` 不能为空
- `models[].id` 不能为空

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
- `cwd` 必须位于主会话 workspace 内
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

## 4.6 POST /api/config/test-model

测试模型 provider 连通性。后端会用给定配置发一个最小请求，消息内容固定为 `"Hi"`。

### 请求体

```json
{
  "baseUrl": "https://api.openai.com/v1",
  "apiKey": "sk-...",
  "api": "openai-completions",
  "modelId": "gpt-4o-mini"
}
```

### 字段说明

- `baseUrl`: 必填
- `apiKey`: 可为空，是否必需由 provider 决定
- `api`: 默认 `openai-completions`
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

## 4.7 POST /api/config/test-mcp

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

## 4.8 GET /api/usage

返回当前主会话的 token 统计。

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

## 4.9 POST /api/upload-images

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

## 4.10 POST /api/shutdown

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

建立连接后，服务端通常会按以下顺序推送初始化事件：

1. `session`
2. `view_state`
3. `history`

## 5.2 客户端 -> 服务端

客户端当前支持 4 类输入。

### 5.2.1 纯文本消息

直接发送字符串：

```text
帮我检查这个仓库的配置问题
```

服务端会将其作为用户消息写入主会话，然后启动一轮 agent 执行。

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

## 5.3.2 一轮主执行中的基础事件

### `start`

表示一轮回复开始。

```json
{
  "type": "start"
}
```

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
3. 收到 `session`、`view_state`、`history` 后初始化 UI
4. 发送纯文本或图片 JSON 消息
5. 处理 `start -> delta/thinking/tool/* -> done`
6. 如需本地上传图片：
   - 先调用 `GET /api/client-config`
   - 再调用 `POST /api/upload-images`
   - 最后把 `url + object_key + attachment_token` 带回 WebSocket 消息

## 8. 已知实现特征

- `/api/config/test-model` 与 `/api/config/test-mcp` 的“联通性失败”通常返回 `200 + {ok:false}`
- `/api/config` 在配置文件语法错误时不会返回 4xx，而是返回可恢复信息
- `/api/sessions` 当前只返回主会话 `main`
- WebSocket 客户端消息没有显式 `type` 字段，按“纯文本 / slash 命令 / JSON 图片消息”三种形态自动分流
- 忙碌时普通文本会进入 deferred intervention 队列，不会立即中断主执行

## 9. 文档维护建议

后续如果新增接口，建议至少同步更新三处：

1. 本文档
2. `frontend/src/types.ts` 或 `frontend/src/types/config.ts`
3. `src/tests/main_tests.rs` 中对应 API / 事件测试
