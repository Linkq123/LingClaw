# LingClaw 后端接口文档

本文档基于当前代码实现整理，覆盖 LingClaw 后端的 HTTP API、WebSocket 协议、鉴权约束、配置结构与常见错误语义。

适用范围：

- 后端入口：[src/main.rs](../src/main.rs)
- WebSocket 输入处理：[src/runtime_loop/socket_input.rs](../src/runtime_loop/socket_input.rs)
- 会话同步与历史回放：[src/socket_sync.rs](../src/socket_sync.rs)、[src/session_store.rs](../src/session_store.rs)
- Session group 与跨 session 控制：[src/session_group.rs](../src/session_group.rs)、[src/session_control.rs](../src/session_control.rs)
- 配置结构：[src/config.rs](../src/config.rs)
- 图片上传：[src/image_uploads.rs](../src/image_uploads.rs)
- 子代理与编排事件：[src/subagents/executor.rs](../src/subagents/executor.rs)、[src/subagents/orchestrator.rs](../src/subagents/orchestrator.rs)

## 1. 总览

- 默认监听地址：`127.0.0.1:18989`
- HTTP 基础地址：`http://127.0.0.1:18989`
- WebSocket 地址：`ws://127.0.0.1:18989/ws`
- 服务框架：`axum`
- 会话模型：默认会话为 `main`，同时支持多个持久化 session 与持久化 session group

后端暴露两类接口：

- HTTP：健康检查、session/group 摘要、session 系统 Skills 开关、session MCP 权限与 catalog、配置读写、todos、模型与 MCP 联通性测试、Usage、图片上传、优雅关停
- WebSocket：聊天主通道与 group chat 通道，承载流式回复、工具事件、推理事件、临时任务计划、子代理事件、编排事件和跨 session 成员事件

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

4. SQLite 保护模式
   - 核心存储运行期发生 I/O、损坏或约束故障后，相关 HTTP 写请求返回 `503 Service Unavailable`
   - body 固定包含 `{ "error": "...", "code": "storage_protected" }`
   - 该状态在当前进程内保持粘性；修复存储后需要重启 LingClaw

### 3.3 会话范围

服务端默认会话为 `main`，同时支持多个持久化 session 和持久化 session group。`/api/sessions` 会返回当前已加载或已持久化的 session 摘要；`/api/session-groups` 返回 group 摘要；普通 WebSocket 连接可通过查询参数 `?session=<id>` 绑定到指定 session，省略时回退到 `main`；group chat 使用 `?group=<id>&session=main` 连接。每个 session 还持有一份当前 `todos` 快照（`revision`、`items[]`、`last_updated_by`、`updated_at`），随会话一起持久化；执行 `/clear` 时会清空 `items[]` 并推进 `revision`，从而拒绝旧的 in-flight 写入。

## 4. HTTP API

## 4.1 GET /api/health

健康检查。

### 响应

```json
{
  "service": "lingclaw",
  "status": "ok",
  "version": "2.x.x",
  "model": "openai/gpt-4o-mini",
  "model_configured": true,
  "sessions": 1,
  "storage": {
    "mode": "healthy"
  }
}
```

### 字段说明

- `service`：固定为 `lingclaw`，供本地客户端确认目标端口运行的是 LingClaw 服务
- `status`：固定为 `ok`
- `version`：后端版本
- `model`：显式配置且当前可解析的默认模型；未配置或已失效时为 `null`
- `model_configured`：`model` 是否已显式配置且当前可解析；首次安装尚未配置模型时为 `false`
- `sessions`：当前内存中会话数量
- `storage.mode`：`healthy` 或 `protected`
- `storage.code`：仅保护模式存在，固定为 `storage_protected`。健康接口本身仍返回 `200`，客户端应检查该字段决定是否允许核心数据写入

## 4.2 GET /api/sessions

返回当前已知 session 摘要列表。

### 响应

```json
{
  "session_ids_case_sensitive": false,
  "sessions": [
    {
      "id": "main",
      "name": "Main",
      "messages": 42,
      "tool_calls": 18,
      "model": "openai/gpt-4o-mini",
      "created_at": 1710000000,
      "updated_at": 1710001234,
      "workspace": {
        "kind": "managed",
        "path": "C:\\Users\\me\\.lingclaw\\main\\workspace",
        "display_name": "workspace",
        "available": true
      }
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
- `session_ids_case_sensitive` 由服务端给出 Session ID 匹配策略：Linux 为 `true`，Windows 为 `false`。客户端应始终优先精确匹配；仅当该字段为 `false` 时再按 ASCII 大小写无关匹配，并使用列表项中的 `id` 作为规范 ID
- `messages` 为会话消息条数
- `tool_calls` 为累计工具调用次数
- 列表同时覆盖默认 `main` 和其他已创建 session
- 每项 `workspace` 表示 Agent 实际使用的工作目录；`kind` 为 `managed` 或 `directory`。`available=false` 时历史仍可读取，但 Agent run 必须在重绑后才能启动
- 可选查询参数 `workspace=<absolute-path>` 按规范化目录精确匹配。Windows 匹配不区分大小写，Linux 精确匹配；查询直接使用 SQLite 工作目录索引

## 4.2.1 POST /api/session

创建一个新的持久化 session。session id 由后端随机生成，格式为 6 位小写英文字母或数字；用户不需要输入 id，可在创建后通过重命名修改显示名称。

### 请求

请求体可省略，此时创建旧版兼容的托管 Session。也可以指定显示名称和工作目录：

```json
{
  "name": "My Project",
  "workspace": {
    "kind": "directory",
    "path": "E:\\work\\project"
  }
}
```

`workspace.kind="managed"` 使用 `~/.lingclaw/<id>/workspace/`。`directory` 要求 `path` 是现存、绝对、可规范化且可表示为 UTF-8 的目录，并且不能位于 LingClaw 私有数据目录 `~/.lingclaw/` 内。

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

- 新 Session 会立即写入 `~/.lingclaw/lingclaw.db`；其 Workspace 仍位于 `~/.lingclaw/<id>/workspace/`
- 对目录 Session 而言，该路径是私有 `session_home`；文件、Shell、Git、图片、Plan evidence 和 MCP roots 使用请求中的外部目录。LingClaw 不会向外部目录写入模板、Persona 或 Memory
- 创建成功后会广播新的 session 列表
- 若随机 id 碰撞，后端会重新生成；连续失败返回 `500`

## 4.2.2 PUT /api/session

修改指定 session 的显示名称和/或工作目录；session id 和私有 Session Home 不变。

### 查询参数

- `session`：可选 session id，省略时使用 `main`

### 请求

```json
{
  "name": "Research Notes",
  "workspace": {
    "kind": "directory",
    "path": "E:\\work\\research"
  }
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
- `workspace` 可省略；`managed` 重新绑定到私有 Session Home，`directory` 使用与创建接口相同的路径校验
- Agent run、排队委托或活动计划存在时禁止重绑，分别返回 `409 session_busy` 或 `409 plan_active`
- 外部目录暂时丢失不影响历史与元数据读取；开始运行返回 `workspace_unavailable`
- 删除 Session 只删除数据库状态和私有 `~/.lingclaw/<id>/`，永不删除外部工作目录
- 保存成功后会持久化 session，并广播新的 session 列表
- 未知 session 返回 `404`，非法名称返回 `400`

## 4.2.3 DELETE /api/session

删除指定的非 Main Session。该接口使用与 `/delete`、WebUI 相同的运行安全检查：活动连接、直接运行、排队或委托任务存在时拒绝删除。

```text
DELETE /api/session?session=<session-id>
```

成功响应：

```json
{
  "ok": true,
  "session_id": "a1b2c3",
  "message": "Deleted session: a1b2c3"
}
```

- 请求必须通过本机 Host/Origin 校验。
- `main` 永远不可删除。
- 删除数据库状态和私有 Session Home；绑定的外部工作目录永不删除。
- 删除成功后广播新的 Session 列表。

## 4.2.4 GET /api/workspaces/browse

本机目录选择器使用的只读接口。仅接受通过本地请求校验的调用。

```text
GET /api/workspaces/browse?path=E%3A%5Cwork
```

响应只包含当前规范化目录、父目录、用户 Home、磁盘根和直接子目录；不返回文件名或文件内容：

```json
{
  "current": "E:\\work",
  "parent": "E:\\",
  "home": "C:\\Users\\me",
  "roots": ["C:\\", "E:\\"],
  "directories": [
    { "name": "project", "path": "E:\\work\\project" }
  ]
}
```

## 4.2.4 Session Group APIs

Session Group 持久化在 `~/.lingclaw/lingclaw.db`，包含 Group 元数据、成员 Session ID、升级管理员、待投票项、群聊消息和成员 run 状态。`main` 是每个 Group 的隐式 owner/admin，不写入成员表，因此不会被 Group dispatch 派发。Group 有独立历史；被派发的成员 Session 也会收到一条固定格式用户消息，内容为 Group 上下文摘要和 Main 指令，因此 Group 历史与成员 Session 历史都会被写入。

Group 功能由 `settings.enableGroups` 显式启用，缺省为 `false`。关闭时本节所有 HTTP 接口统一返回：

```json
{
  "code": "group_feature_disabled",
  "error": "Group chat is disabled by configuration."
}
```

状态码为 `403 Forbidden`；已有 Group 数据不会删除。

### GET /api/session-groups

返回已知 group 摘要列表，按 `updated_at` 倒序排列。

```json
{
  "groups": [
    {
      "id": "a1b2c3",
      "name": "Review Group",
      "members": 2,
      "messages": 4,
      "running": 1,
      "created_at": 1710000000,
      "updated_at": 1710000300,
      "corrupt": false
    }
  ]
}
```

### POST /api/session-group

创建 group。`id` 由后端生成；`members[]` 会去重并丢弃非法 session id。

```json
{
  "name": "Review Group",
  "members": ["worker-a", "worker-b"]
}
```

成功响应：

```json
{
  "ok": true,
  "group": {
    "id": "a1b2c3",
    "name": "Review Group",
    "members": 2,
    "messages": 0,
    "running": 0,
    "created_at": 1710000000,
    "updated_at": 1710000000,
    "corrupt": false
  }
}
```

### GET /api/session-group?group=<id>

返回完整 group：

```json
{
  "group": {
    "id": "a1b2c3",
    "name": "Review Group",
    "members": ["worker-a", "worker-b"],
    "explicitPrimaryModelConfigured": false,
    "model_override_members": ["worker-a"],
    "model_configured_members": ["worker-a"],
    "configRevision": 1720684800124,
    "admins": ["worker-b"],
    "pending_votes": [],
    "member_details": [
      { "id": "main", "name": "Main", "role": "owner" },
      { "id": "worker-a", "name": "Worker A", "role": "member" },
      { "id": "worker-b", "name": "Worker B", "role": "admin" }
    ],
    "messages": [],
    "runs": [],
    "created_at": 1710000000,
    "updated_at": 1710000000,
    "version": 2
  }
}
```

### PUT /api/session-group?group=<id>

更新 group 名称和成员列表。`name` 可省略；`members` 是完整替换列表。

```json
{
  "name": "Backend Review",
  "members": ["worker-a"]
}
```

### DELETE /api/session-group?group=<id>

在 SQLite 事务中删除 Group 及其关联的成员、投票、消息和运行记录，然后广播新的 Group 列表。不存在返回 `404`。如果 Group 里仍有 `queued` 或 `running` 的成员 run，后端会返回 `409 Conflict`；调用方需要先通过 Group socket 发送 `{"type":"group_stop"}` 或调用 `session_control.stop` 停止这些 run，再删除 Group。

### PUT /api/session-group/member?group=<id>&session=<session-id>

把成员 session 升级为 group admin。`main` 是隐式 owner，不需要也不能升级。成功响应返回完整 group JSON，并通过 group socket 广播新的 `group` payload。

### DELETE /api/session-group/member?group=<id>&session=<session-id>

用户 UI 以 owner 视角直接移除 group 成员，同时把该成员从 `members[]`、`admins[]`、相关 `pending_votes[]` 中移除，并停止该成员在本 group 内仍处于 `queued/running` 的 run。通过 `session_control.remove_group_member` 且带非 `main` 的 `requester_session_id` 时，会按升级管理员 2/3 投票规则处理。

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

返回前端和 TUI 运行所需的轻量配置，包括功能发现、图片上传 token 和当前 S3 配置身份。客户端应在读取 Group 数据或建立 Group socket 前先读取该接口。

### 响应

```json
{
  "upload_token": "...",
  "s3_config_id": "...",
  "features": {
    "groups": false
  },
  "protocols": {
    "execution_identity": 1
  }
}
```

### 说明

- 仅本地请求可访问
- `upload_token` 由前端拿到后用于 `POST /api/upload-images`
- `s3_config_id` 是不包含明文凭据的当前 S3 配置身份；未配置 S3 时为 `null`
- `features.groups` 是当前热更新的 Group 开关；字段缺失的旧服务端应按 `false` 处理
- WebUI 的发送门禁同时要求当前连接意图已成功协商协议、准确的 socket generation 处于 OPEN、当前 Session/Group 身份与历史已准备完成，以及原有模型、Group 目标、存储、上传和身份锁条件通过。Send、Enter、Plan、Stop 与 socket slash 命令共享这一事实；模型无关命令不能绕过传输门禁。协商中、会话准备中、离线或协议 fail-close 时，按钮保持中性禁用，并提供简短本地化状态与完整无障碍说明。连接失败可在同页显式重新连接；同一 Session 的首次重连历史重放保留仍有效的待发送附件，实际目标切换和普通清空历史继续清理附件。重连不排队、不自动重发、不伪造用户消息；恢复后仅用户再次明确提交才发送一个 frame。旧连接 open/close/error/message 与迟到协商或模型 HTTP 结果不得解锁或关闭新连接。
- `protocols.execution_identity = 1` 表示顶层 `start`、终态 `error` 和 `done` 提供服务器运行身份。WebUI/TUI 必须在**每一个** socket generation 建立前重新读取该字段，并把响应绑定到当次连接意图及 Session/Group 目标；迟到的旧协商不得覆盖新目标或创建 WebSocket。WebUI 的读取使用覆盖响应头和完整 JSON body 的统一硬截止，并由每个连接意图独立持有 AbortController；新意图、取消、目标切换或协议 fail-close 会主动 abort 旧请求。Bootstrap 不再发起独立的首次特性请求，而是由首个连接意图用同一次 `/api/client-config` 响应完成特性应用、目标复核/重定向与 WebSocket 创建；任一步骤前 token 或目标失效都必须放弃。Group close 恢复探测使用独立的有界 owner，不会取消合法连接意图。值为 `1` 时使用严格双身份门禁；字段缺失明确表示 legacy daemon，只能在首个、未重连的 socket generation 内把无身份终态绑定到同连接的无身份 `start`
- legacy socket 一旦断开、Session/Group 切换需要第二连接，客户端必须清除 busy/run 状态并 fail-closed，显示刷新客户端或以当前版本重启 daemon 的提示；TUI 还会立即恢复尚未由 History 确认的文本、附件和 Plan mode 草稿。未知的非空协议版本、HTTP/JSON 失败或与当前连接意图不匹配的协商响应同样在网络连接前拒绝，且不得造成重试风暴、假在线或永久 busy。daemon 从 legacy 升级为当前严格协议后，后续显式连接可由新协商安全恢复
- 严格协议连接若收到缺少 `run_connection_id` 的顶层 `start`，属于连接级协议错误：客户端必须关闭产生该事件的准确 socket generation，撤销发送前的乐观 busy/stream/ReAct/timer/Plan action 状态并显示恢复提示。若该事件发生在已经渲染过程步骤的精确活动 run，WebUI 会先把同一身份栈收口为可恢复的 `incomplete`、保留用户手动展开状态，再清除 active 指针；不得留下 running DOM/ARIA 状态，也不得改写新 generation、Group busy、历史或无关终态栈。TUI 只能在身份验证成功后确认 pending outbound；缺身份终态不得认领新 run，非终态 `error` 则继续只作为错误提示
- TUI 协商发现 Groups 从关闭变为开启时，先完成本次 WebSocket 握手，再异步获取 `/api/session-groups`；每个请求绑定 socket generation、Session/Group 目标和独立 feature-cycle token。每次 enable/disable 状态变化或目标重置都会更换 token；重复的 `enabled=true` 不会开启新周期或重复请求。token 采用仍被旧任务/结果持有的分配身份而非可 wrap 整数，因此同一 socket、同一目标 disable→re-enable 后，旧周期响应绝不可能与新周期相等。首次短暂失败会在同一健康 socket 上执行有限退避重试（当前最多三次总尝试），同一完整绑定最多一个请求在途；绑定变化会失效旧 timer/retry/result，新 generation 可立即建立自己的刷新。只有完整绑定仍为当前且 Groups 仍开启时才应用结果，迟到响应不得清除新周期的 pending/in-flight、消耗 attempt、改写 status 或列表，也不得阻塞连接 preflight 或形成忙循环
- 客户端必须把 token 与同一次有效配置身份一起使用；强制刷新产生的新响应应覆盖旧的并发响应

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
  "environmentModelConfigured": false,
  "explicitPrimaryModelConfigured": false,
  "configuredModelsAvailable": false,
  "configRevision": 1720684800123,
  "configFileEtag": "4d2f0a9f4c8a0e7b4f51b6d61ce1c56b9f80fbfe13a4b3662bce289b55b6905f",
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
  "environmentModelConfigured": false,
  "explicitPrimaryModelConfigured": false,
  "configuredModelsAvailable": false,
  "configRevision": 1720684800123,
  "configFileEtag": "0ab1e83a02170e78445c84fa95708d8a7c3553e56911c95d531f0d21ab4d8408",
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
- `environmentModelConfigured`：是否通过非空 `LINGCLAW_MODEL` 显式配置了运行时主模型；不包含内置默认模型回退
- `explicitPrimaryModelConfigured`：服务端对当前运行时配置完成清洗和引用校验后，是否存在显式全局主模型；前端必须使用该字段判断可运行性，不能直接信任原始 JSON 中的 `primary`
- `configuredModelsAvailable`：运行时完成环境变量展开和 provider 清洗后，provider 目录中是否仍存在至少一个模型；不包含 legacy `LINGCLAW_MODEL` 主模型或空目录动态 provider。前端用它区分“模型未配置”和“Agent 模型未配置”，不能从原始 `config` 推断
- `configRevision`：JavaScript 安全整数范围内的模型配置修订号。相对于当前 LingClaw 进程通过 `PUT /api/config` 执行的更新，配置文件内容、运行时模型状态和该修订号在同一把锁下取样，因此不会混合一次进程内更新前后的状态。手工编辑或其他进程写入不经过这把锁：后续 `GET` 可能先看到新的磁盘 `raw`/`configFileEtag`，但当前进程不会自动热加载该配置；需通过 Settings 重新保存或重启 LingClaw 才会应用到运行时
- `configFileEtag`：原始配置文件内容的 SHA-256。Settings 保存整份配置时应把它作为 `baseConfigFileEtag` 回传，用于检测其他页面、进程或手工编辑造成的并发修改；它与会被 Session `/model` 推进的 `configRevision` 相互独立
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
- 刷新 MCP server tools/resources/prompts 缓存与运行时状态

### 请求体

顶层必须包含 `config` 字段。Settings 等“先读后整份保存”的客户端还应携带上次 GET 返回的可选 `baseConfigFileEtag`。可选 `session` 指定此次 MCP `cwd` 校验所使用的 Session 工作目录；缺失时使用当前已加载 Main 的工作目录，并为旧客户端保留托管 Main 目录回退：

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
      "enableTaskPlan": false,
      "enableGroups": false,
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
  },
  "session": "main",
  "baseConfigFileEtag": "4d2f0a9f4c8a0e7b4f51b6d61ce1c56b9f80fbfe13a4b3662bce289b55b6905f"
}
```

### 成功响应

```json
{
  "ok": true,
  "environmentModelConfigured": false,
  "explicitPrimaryModelConfigured": true,
  "configuredModelsAvailable": true,
  "configRevision": 1720684800124,
  "configFileEtag": "bc72e9017a83d5f7d26bc216e8ef26e73ace9dbe0d987f748de58fe532a3119d"
}
```

三个模型状态字段的语义与 `GET /api/config` 相同，供保存配置后的前端即时刷新模型可用状态。保存完成后，服务端也会向所有已连接 Session/Group 推送具有相同 `configRevision` 的更新状态。

`configRevision` 是进程内严格递增、以 Unix epoch 毫秒为初始种子的模型配置状态序号：每次成功应用 `PUT /api/config`，以及每次成功持久化 Session 模型/Effort 偏好（包括 `/model`、`/think` 和 `PUT /api/session-models`）都会推进它。时间种子使它通常也能跨重启递增，但协议只保证单个服务端进程内的严格单调性。前端应持续保留已接受的最大序号并忽略更小的异步状态包；普通 WebSocket 重连不应无条件清零该比较状态。若新连接收到的首个带版本 Session/Group 模型状态小于当前基线，客户端可将其视为后端进程重启并建立新基线；HTTP 响应不能消费这次连接握手。

若 `baseConfigFileEtag` 与写锁内重新读取到的文件内容不一致，服务端返回 `409 Conflict`，不会写文件或热重载。响应包含当前 `config`（若仍可解析）、`configRevision` 与 `configFileEtag`；客户端应保留本地编辑并明确让用户重新加载，而不是自动覆盖任一版本。只执行 Session `/model` 不会改变文件 ETag，因此不会产生无关的 Settings 保存冲突。

当配置包含 MCP `cwd` 时，服务端会按 `session` 对应的持久化 `working_directory` 执行路径边界校验。显式 Session 不存在时返回 `404`；`session` 不是非空字符串时返回 `400`。这不会把 MCP policy 或其他 LingClaw 私有数据写入外部工作目录。

```json
{
  "error": "Configuration changed after it was loaded. Reload the latest configuration before saving.",
  "config": {},
  "configRevision": 1720684800125,
  "configFileEtag": "bc72e9017a83d5f7d26bc216e8ef26e73ace9dbe0d987f748de58fe532a3119d"
}
```

### 校验规则摘要

#### settings

- `port`: `u16`
- `execTimeout`, `toolTimeout`, `subAgentTimeout`: 秒
- `subAgentTimeout = 0` 表示不限时
- `maxLlmRetries`: 非负整数
- `enableStateDigest` 默认可开启
- `enableTaskPlan` 默认关闭；界面名称为“自动执行提纲”。开启后仅在没有批准计划的普通 Execute run 中生成 `TaskPlan`、注入 `## Task Plan` 动态上下文并发送 `task_plan` live event；Plan-only 与批准计划执行期间抑制
- `enableGroups` 默认关闭；热关闭会停止活动 Group run、广播 `feature_status` 并断开 Group socket，但不删除持久化 Group 数据

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
      "effort": {
        "levels": ["auto", "low", "medium", "high"],
        "default": "medium"
      },
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
- `models[].effort.levels` 如提供，必须是固定集合 `auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max` 的非空、无重复子集；保存时按该固定顺序规范化
- `models[].effort.default` 必须存在于 `levels`；除 `off` 外的 Effort 要求 `models[].reasoning = true`
- 未配置 `effort` 的旧推理模型兼容完整集合并默认 `auto`；非推理模型等效为仅支持 `off`
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
- 如果该 provider 已定义非空 `models` 列表，则 `model-id` 必须存在；空列表继续允许显式 `provider/model-id` 兼容动态模型目录
- 当 `models.providers` 非空且 JSON 配置使用纯 model ID 时，该 ID 必须在所有 provider 的非空模型目录中唯一命中；未知或歧义 ID 会使 `PUT /api/config` 返回 `400`
- 通过环境变量 `LINGCLAW_MODEL` 提供的 legacy 纯 model ID 保留宽松兼容，不要求进入 JSON provider 目录；这项例外不适用于 Settings 写入的 `agents.defaults.model.*`
- 若 JSON 原本声明了 provider，但运行时环境变量展开/校验后所有 provider 都不可用，服务端会保留“目录曾声明”的状态：已有 Session override 与新的 `/model` 选择均不得退化为 builtin 前缀或纯 ID legacy 路由；仅单独显式配置且与当前值完全一致的 `LINGCLAW_MODEL` 仍可使用
- 当 `models.providers` 为空时，允许使用内置 provider 前缀：
  - `openai`
  - `openai-responses`
  - `anthropic`
  - `ollama`
  - `gemini`

#### mcpServers

每个 MCP server 项结构：

```json
{
  "transport": "stdio | streamable-http",
  "command": "string",
  "url": "https://example.com/mcp",
  "args": ["string"],
  "env": {
    "KEY": "VALUE"
  },
  "headers": {
    "Authorization": "Bearer ${TOKEN}"
  },
  "auth": {
    "clientId": "${MCP_CLIENT_ID}",
    "clientSecret": "${MCP_CLIENT_SECRET}",
    "scopes": ["read"]
  },
  "cwd": ".",
  "enabled": true,
  "timeoutSecs": 30
}
```

约束：

- `transport` 可选，支持 `stdio` 和 `streamable-http`
- 未写 `transport` 且有 `command` 时按 `stdio`；没有 `command` 但有 `url` 时按 `streamable-http`；两者都没有时按 `stdio`
- `stdio` server 的 `command` 不能为空
- `streamable-http` server 的 `url` 不能为空，并且必须以 `http://` 或 `https://` 开头
- `env` 值、`headers` 值、`auth.clientId`、`auth.clientSecret` 支持精确 `${ENV_NAME}` 占位符
- `timeoutSecs` 不能为 `0`
- `cwd` 必须位于当前配置测试所使用 session 的 workspace 内
- `cwd` 不允许逃逸 workspace
- `cwd` 不允许穿过受保护 symlink
- `cwd` 不允许指向 `.lingclaw-bootstrap`
- `mcpServers` 只声明 server；server/tool 是否注入模型由每个 session 的 `/api/mcp/session-policy` 控制，默认不注入任何 MCP tool

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

## 4.6.1 GET/PUT /api/session-models

读取或原子更新一个普通 Session 的有效模型与 Thinking Effort。省略 `session` 时使用 `main`；Group 不使用此接口，因为各成员保留自己的模型路由。

### GET

```http
GET /api/session-models?session=main
```

```json
{
  "session": {
    "id": "main",
    "model": "openai/gpt-5.5",
    "effort": "medium",
    "modelOverridePresent": true,
    "modelOverrideConfigured": true,
    "effectiveModelConfigured": true
  },
  "explicitPrimaryModelConfigured": true,
  "capabilities": {
    "image": true
  },
  "models": [
    {
      "ref": "openai/gpt-5.5",
      "provider": "openai",
      "id": "gpt-5.5",
      "name": "GPT-5.5",
      "input": ["text", "image"],
      "reasoning": true,
      "efforts": ["auto", "low", "medium", "high", "xhigh"],
      "defaultEffort": "medium"
    }
  ],
  "configRevision": 1720684800125
}
```

模型目录只包含展示、能力和 Effort 元数据，不返回 `apiKey`、`baseUrl`、Provider headers 或其他密钥。

### PUT

```http
PUT /api/session-models?session=main
Content-Type: application/json

{
  "model": "openai/gpt-5.5",
  "effort": "high"
}
```

服务端在同一持久化操作中更新 `model_override` 与 `think_level`，成功前不会向 Agent run 暴露中间组合。运行中、Session 切换或存储保护时不得切换。

```json
{
  "ok": true,
  "session": {
    "id": "main",
    "model": "openai/gpt-5.5",
    "effort": "high",
    "modelOverridePresent": true,
    "modelOverrideConfigured": true,
    "effectiveModelConfigured": true
  },
  "explicitPrimaryModelConfigured": true,
  "capabilities": {
    "image": true
  },
  "configRevision": 1720684800126
}
```

`capabilities.image` 与已提交模型来自同一个运行时配置快照，客户端可立即同步图片入口，无需等待后续 WebSocket 广播。成功后服务端推进 `configRevision`，并通过 `session_model_configuration` / `group_model_configuration` 刷新相关客户端。稳定错误码：

- `model_unavailable`：模型引用不存在或当前不可用，`400`
- `effort_not_supported`：目标模型不允许该 Effort，`400`
- `session_busy`：Session 已有活动 run，`409`
- `session_not_found`：Session 不存在，`404`
- `storage_protected`：SQLite 处于保护模式，`503`

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

测试 MCP server 是否可连接并完成 tools 列表发现。支持 stdio 与 Streamable HTTP。

查询参数：

- `session`：可选 session id，省略时使用 `main`；用于按该 session workspace 解析 `cwd` 和 MCP roots

### 请求体

```json
{
  "server": "filesystem",
  "transport": "stdio",
  "command": "uvx",
  "args": ["mcp-server-filesystem"],
  "env": {
    "DEBUG": "1"
  },
  "cwd": ".",
  "timeoutSecs": 30
}
```

Streamable HTTP 示例：

```json
{
  "server": "remote",
  "transport": "streamable-http",
  "url": "https://example.com/mcp",
  "headers": {
    "X-API-Key": "${REMOTE_MCP_API_KEY}"
  },
  "auth": {
    "clientId": "${REMOTE_MCP_CLIENT_ID}",
    "clientSecret": "${REMOTE_MCP_CLIENT_SECRET}",
    "scopes": ["read"]
  },
  "timeoutSecs": 30
}
```

- `server` 可选；提供后会按该 server 名称复用 `~/.lingclaw/mcp-auth.json` 中已有的 OAuth token。省略时使用临时测试名称，不会匹配已保存的授权状态。
- `auth` 可选，形状与 `mcpServers.<name>.auth` 相同，用于测试 Streamable HTTP OAuth 客户端配置。

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

## 4.8.1 Session MCP APIs

这些接口管理当前 session 的 MCP server/tool 权限，并提供 resources/prompts 的只读浏览。`mcpServers` 配置只负责声明 server；默认不会把任何 MCP tool 注入模型，必须通过 `PUT /api/mcp/session-policy` 为该 session 手动启用。

### GET /api/mcp/catalog

查询参数：

- `session`：可选 session id，省略时使用 `main`

响应示例：

```json
{
  "session": {"id": "main", "name": "Main"},
  "policy": {
    "enabledServers": ["filesystem"],
    "enabledTools": ["mcp__filesystem__read_file__abcd1234"],
    "confirmMutatingTools": false,
    "clientCapabilities": {"roots": false, "sampling": false, "elicitation": false}
  },
  "servers": [
    {
      "id": "filesystem",
      "transport": "stdio",
      "configuredEnabled": true,
      "enabled": true,
      "authenticated": false,
      "toolCount": 1,
      "resourceCount": 0,
      "promptCount": 0,
      "error": null
    }
  ],
  "tools": [
    {
      "id": "mcp__filesystem__read_file__abcd1234",
      "server": "filesystem",
      "rawName": "read_file",
      "description": "Read a file",
      "readOnly": true,
      "enabled": true
    }
  ],
  "resources": [],
  "prompts": []
}
```

### PUT /api/mcp/session-policy

查询参数：

- `session`：可选 session id，省略时使用 `main`

请求体：

```json
{
  "enabledServers": ["filesystem"],
  "enabledTools": ["mcp__filesystem__read_file__abcd1234"],
  "confirmMutatingTools": true,
  "clientCapabilities": {"roots": true, "sampling": false, "elicitation": false}
}
```

说明：

- `enabledServers` 必须是已配置且未禁用的 server
- `enabledTools` 必须是当前发现到的 MCP tool，且所属 server 必须在 `enabledServers` 中
- `confirmMutatingTools` 启用后，LingClaw 会阻止自动执行启发式判定为 mutating 的 MCP tool，避免模型绕过确认直接修改外部系统
- `clientCapabilities.roots` 仅控制 stdio transport 在 initialize 时是否声明 `roots` capability。stdio 的 `roots/list` 返回受目录 capability 约束的 root：Linux/Android 只让目标 MCP 子进程继承 fd，父进程副本保持 close-on-exec；Windows 在响应/Session 生命周期内保持 no-delete 根句柄链。Streamable HTTP 无法安全传递本地 OS capability，因此总是不声明 `roots`，并以 JSON-RPC `-32601` 拒绝服务器发来的 `roots/list`
- `sampling` / `elicitation` 字段为兼容预留，后端不会声明尚未实现的 client capability
- 保存到当前 session workspace 的 `.lingclaw-mcp-policy.json`
- 子代理只会继承该 session 已启用的 MCP tools，再按子代理 `mcp_policy` 做过滤

### POST /api/mcp/resource/read

查询参数：

- `session`：可选 session id，省略时使用 `main`

请求体：

```json
{
  "server": "filesystem",
  "uri": "file:///workspace/README.md"
}
```

响应中的 `result` 是 MCP server 原始 `resources/read` result。请求的 server 必须已在当前 session 的 MCP policy 中启用。该接口不会自动写入 system prompt 或对话历史，前端只提供预览/手动插入。

### POST /api/mcp/prompt/get

查询参数：

- `session`：可选 session id，省略时使用 `main`

请求体：

```json
{
  "server": "docs",
  "name": "summarize",
  "arguments": {"topic": "deployment"}
}
```

响应中的 `result` 是 MCP server 原始 `prompts/get` result。请求的 server 必须已在当前 session 的 MCP policy 中启用。该接口同样只用于用户手动浏览和插入。

### OAuth

- `POST /api/mcp/auth/start`
- `GET/POST /api/mcp/auth/callback`
- `POST /api/mcp/auth/disconnect`

`auth/start` 请求体：

```json
{
  "server": "remote-docs"
}
```

成功响应：

```json
{
  "ok": true,
  "server": "remote-docs",
  "authorizationUrl": "https://auth.example.com/authorize?...",
  "redirectUri": "http://127.0.0.1:18989/api/mcp/auth/callback",
  "clientId": "client-id",
  "scopes": ["read"]
}
```

`auth/callback` 支持浏览器 GET query：

```text
/api/mcp/auth/callback?server=remote-docs&code=...&state=...
```

也支持前端 POST：

```json
{
  "server": "remote-docs",
  "code": "authorization-code",
  "state": "oauth-state"
}
```

`auth/disconnect` 请求体：

```json
{
  "server": "remote-docs"
}
```

`auth/start` 会发现 OAuth protected resource metadata 与 authorization server metadata，生成 PKCE 授权 URL，并把 pending state 写入本地授权文件。若授权服务器不支持动态客户端注册，需要在 `mcpServers.<name>.auth.clientId` 中配置客户端 ID；`clientSecret` 可选。callback 支持浏览器 loopback GET，也支持前端 POST `{server, code, state}` 完成 token exchange。OAuth token 存储在本地 `~/.lingclaw/mcp-auth.json`，按 server 分组保存；access token 过期时会使用 refresh token 自动刷新；`auth/disconnect` 会清除对应 server 的本地 token。

Streamable HTTP 运行时会在 initialize 后维护 GET SSE 通知流，并记录 `Last-Event-ID` 用于后续重连；POST/GET SSE 中的 `notifications/tools/list_changed`、`notifications/resources/list_changed`、`notifications/prompts/list_changed` 会分别清理对应缓存。每个完整 Session cache key 使用一个稳定的 per-key request control：Settings 保存、授权断开和 server 级清理会推进 request epoch，但不会拆掉仍有在途 lease 的锁；迟到 initialize 响应只有在 authority 仍为当前且 key 尚未被新 Session 占用时才能安装，普通 POST 还必须匹配发出请求时的 `session_id + generation`。普通缓存 Session、临时 one-shot（包括 catalog/policy discovery 与隔离工具调用）、迟到响应，以及 SSE/workspace/idle/Settings/server 清理的所有 Streamable HTTP initialize 与 DELETE，统一共享按配置 endpoint 规范化的 remote cleanup-domain authority。规范化去除 URL userinfo 和 fragment，仅解码百分号编码的 RFC unreserved octet，把保留的百分号编码统一成大写十六进制，并在保留 scheme、host、有效 port、path 与 query 的同时维持 reserved 编码差异；非法百分号编码稳定 fail-closed。transport POST/GET SSE/DELETE client 使用同一规范 endpoint 且禁止重定向，OAuth discovery/token client 则保持独立 redirect policy。timeout、HTTP 不使用的 command/args/env/cwd、workspace、policy namespace、client capabilities、本地 server 别名、headers、轮换凭据及其他本地 cache 维度都不会划分该域；同 endpoint 的不同本地认证配置也保守共享该域，而不同 endpoint 仍可并行。initialize 在请求可能发出前启用 RAII owner；调用者取消或 panic、超时、Settings/server 失效、未知响应及相关任务取消都会留下进程期 tombstone。临时 Session 在 initialize handoff 后继续携带 lifecycle owner，贯穿请求与 shutdown；终态清理前的 Drop/取消会同步隔离 endpoint，并移除本地 Session/event/stream 状态。若失败的 initialize 响应、JSON-RPC error、后置解析或 `notifications/initialized` 已提供 Session ID，运行时会在同一域受控 DELETE；只有确认完成或确认未应用才重新授权。远端 DELETE cleanup 跨实际网络副作用持有串行 authority；DELETE 发送前记录 pending tombstone 并脱离旧本地 generation，调用者取消会保留 pending/uncertain 隔离，且不会取消后台请求。只有 `200 OK` 或 `204 No Content` 能确认 DELETE 已完成；按 MCP/HTTP 语义，`404` 表示 Session 已不存在、`405` 表示服务端不支持客户端终止、`410` 表示目标已消失，三者都不会留下迟到 DELETE。`202 Accepted`、其他任何状态、超时、网络失败或后台任务取消会保留 uncertain tombstone，在当前进程内拒绝同 endpoint initialize，而无关 endpoint 不受影响。endpoint authority 跨全部 cache key 追踪活动身份：新 key 安装同一个 Session ID 时，会在统一运行时事务中推进旧 key epoch、移除旧 Session/event ID/SSE task，并清理对应 tool/resource/prompt descriptor cache；旧 key 同时获得 supersession 门禁，之后的请求会在网络发送前被拒绝，迟到响应也无法恢复状态。replacement 仍有效时，旧清理仅清除本地代际，不发送远端 DELETE。idle TTL 到期不会再同步丢弃本地 identity；异步调用方会持有 endpoint 与 cache-key authority 完成受控 DELETE，仅在 `Confirmed`/`NotApplied` 后重建，并让并发调用共享同一清理/重建流程；`202`、超时、取消或其他 ambiguous 结果都在发送 initialize 前 fail-closed。已确认或明确未应用的 cleanup 会在 Session、event ID、stream、请求、cleanup lease 与 endpoint mapping 全部结束后回收空 per-key/endpoint control。

普通 Session-bound POST 在最终发送前校验通过后登记 `cache key + epoch + generation` 的 RAII in-flight 计数，并一直持有到响应处理、传输错误、timeout、调用取消或 panic 路径结束。idle 检测只会在该精确代际计数为零时进入 DELETE；计数非零时会复活/延后过期时间，旧代际 Drop 不会影响 replacement。响应进入 workspace/identity/404 终止失败后，清理先仅释放这个已完成响应自己的 lease，再原子写入 endpoint 与 cache-key Pending quarantine、脱离旧本地 generation，并让 runtime-owned task 等待其余同代际请求归零；最后一个正常完成的 lease 只触发一次 DELETE。请求取消或 cleanup task 取消会把 cleanup 强制转为 Uncertain，即使迟到 DELETE 返回成功也不解除隔离。传输、响应体或 SSE timeout 会在任何 best-effort `notifications/cancelled` token 获取或网络 await 之前，同步写入 endpoint Uncertain tombstone、移除 exact generation 并释放该请求 lease，因为远端 POST 仍可能完成；该规则覆盖普通缓存请求、临时 one-shot 请求，以及尚无 Session ID 的 one-shot initialize。若 endpoint 已有 Pending cleanup，timeout 会原子、单调地把同一 tombstone 升级为 sticky Uncertain；旧 DELETE observer 的 `200/204/404/405/410`、后续 lifecycle cleanup、通知结果或调用者取消都不能降低或清除它。发送 DELETE 前再次发现同 endpoint、同 Session ID replacement 时只结束旧本地清理，不触碰 replacement；不同 endpoint 可并行。initialize 响应安装 Session ID 后，initialize owner 立即绑定该精确 identity；若 `notifications/initialized`、SSE 启动或其他后置检查完成前失效，会原子写入 endpoint quarantine，并按 generation 移除 Session、Last-Event-ID、SSE task 与 descriptor 状态。即使本地 Session 仍显示 Active，Pending/Uncertain cleanup 也会在任何后续 POST 前被拒绝。

HTTP descriptor cache entry 绑定 per-key control epoch、可选 `session_id + generation` 与独立 descriptor epoch。tools/resources/prompts 的直接列表和 catalog 批量加载在请求前捕获 authority，响应后仅通过同一 runtime mutex 下的 CAS 才能写缓存；cache hit 同样重新验证。same-ID replacement、Settings/授权/server 清理或任一 `notifications/*/list_changed` 会推进相应 authority，因此旧响应即使已经完成网络读取，也不能在失效之后重新填回 tool/resource/prompt/catalog 缓存。该机制只增加内部一致性约束，不改变公开 MCP JSON-RPC 协议。

## 4.9 GET /api/usage

返回指定 Session 的 Token 统计。

### 查询参数

- `session=<id>`（可选）：指定要查询的 Session；省略时默认查询 `main`
- Session ID 不合法时返回 `400 Bad Request`
- 有效 Session 尚未载入内存时，后端会尝试从磁盘恢复；找不到时返回字段完整的零值统计，而不是 `404`

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
- `input_source` / `output_source`：最近一次统计更新的来源，常见值为 `provider`、`estimated`；不表示累计统计的整体精度
- `source_scope`: 当前固定为 `latest_update`，用于明确上述来源字段的作用范围
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
      "attachment_token": "...",
      "s3_config_id": "..."
    }
  ],
  "urls": [
    "https://...presigned..."
  ],
  "errors": [],
  "s3_config_id": "..."
}
```

### 字段说明

- `images`: 推荐前端保存，包含可信上传元信息
- `urls`: 仅 URL 列表，便于兼容旧逻辑
- `errors`: 局部失败列表；即使某些文件失败，其他文件仍可成功
- 顶层和每个 `images[]` 中的 `s3_config_id` 必须一致，并与上传开始时的当前配置身份相同；不一致的响应不可加入待发送附件

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
  ],
  "s3_config_id": "..."
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

也可以进入指定 group chat（前端使用 `session=main` 连接参数）：

```text
ws://127.0.0.1:18989/ws?group=a1b2c3&session=main
```

- 普通 session socket 省略 `session` 时默认绑定 `main`；group socket 必须显式传 `session=main` 作为前端 group 控制 UI 的连接要求
- 指定的 session 不存在时，服务端会按该 id 创建新 session（前提是 id 合法）
- 非法 session id，以及已持久化但损坏或当前无法加载的 session，都会回退到 `main`，并额外推送一条 `error` 事件说明原因
- `group` 存在时进入 group chat，不会自动创建 group；`settings.enableGroups=false` 时在升级前返回 `403 group_feature_disabled`。未知/非法 group 或未携带 `session=main` 的 group socket 会返回 `error` 并关闭。`session=main` 是 UI 防误用约束，不是浏览器/本地进程之间的安全授权边界

建立连接后，服务端通常会按以下顺序推送初始化事件：

1. `session`
2. `view_state`
3. `todos_state`
4. `history`
5. `session_group_list`

Group socket 初始化顺序通常为：

1. `group`
2. `group_history`
3. `session_group_list`
4. `session_list`

## 5.2 客户端 -> 服务端

客户端当前支持普通 session 输入、计划执行输入、忙碌期干预和 group chat 输入。浏览器前端的普通消息会发送 JSON 字符串，以便携带本轮运行选项；旧客户端仍可直接发送纯文本。

### 5.2.1 纯文本消息

直接发送字符串：

```text
帮我检查这个仓库的配置问题
```

服务端会将其作为当前 WebSocket 绑定 session 的用户消息，然后启动一轮 agent 执行。

也可以发送结构化 JSON 字符串：

```json
{
  "text": "帮我检查这个仓库的配置问题",
  "plan_mode": false
}
```

`plan_mode` 为可选布尔值：`true` 表示为当前 Session 启动新的结构化计划流程，服务端只允许只读探索工具，并要求模型通过内部 `submit_plan` 返回 `needs_input` 或 `ready`；`false` 或省略表示直接进入正常执行模式。计划流程使用 `plan_id + revision` 乐观并发，状态通过 `plan_state` 持续同步。

每个 Session 同时只允许一个 `planning`、`needs_input`、`ready` 或 `executing` 计划。存在活动计划时，新的普通 Execute 消息会以 `plan_already_active` 拒绝；客户端应先执行、修订或丢弃当前计划。Group socket 不支持 `plan_mode`。

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
- `/new` 可能调用上下文压缩模型，因此与普通消息、图片和 `execute_plan_id` 一样，服务端要求全局显式主模型或当前 Session `/model` override；无模型命令仍可正常使用
- Slash 命令只适用于普通 session socket；连接到 `/ws?group=<id>&session=main` 时，`/...` 文本会被拒绝，不会作为 group 消息派发

### 5.2.3 图片消息 JSON

当携带图片时，发送 JSON 字符串：

```json
{
  "text": "请分析这张图",
  "plan_mode": true,
  "images": [
    {
      "url": "https://...",
      "object_key": "lingclaw/images/2026-05-02/....png",
      "attachment_token": "...",
      "s3_config_id": "..."
    }
  ]
}
```

`images[]` 元素结构：

```json
{
  "url": "https://...",
  "object_key": "optional",
  "attachment_token": "optional",
  "s3_config_id": "optional"
}
```

说明：

- `object_key + attachment_token + s3_config_id` 三者成组使用；缺少任一字段都会被拒绝
- 若三者都存在且签名及配置身份仍匹配，服务端会把该图当作受信任的已上传对象
- 若只传 `url`，服务端会按普通远程图片 URL 校验
- 最多 `10` 张图
- `plan_mode` 语义同普通消息 JSON；开启时只生成计划，关闭或省略时直接执行

### 5.2.4 计划操作

结构化计划的统一请求格式：

```json
{
  "plan_action": {
    "action": "execute",
    "plan_id": "plan_...",
    "revision": 2,
    "allow_stale": false
  }
}
```

`action` 支持：

- `feedback`：回答问题或请求修订；可附带 `text`，以及 `{question_id: answer}` 形式的 `answers`
- `execute`：执行 `ready` revision；本地证据过期时，用户确认后必须同时回传 `allow_stale: true` 和最近一次 `plan_stale.confirmation_token`
- `refresh`：重新检查非终态、非执行中的计划并生成新 revision
- `discard`：丢弃非终态、非执行中的计划
- `resume`：从已批准且至少开始过一次执行的 `failed` 或 `stopped` 计划继续剩余步骤；规划阶段中断产生的 `stopped` 计划不允许 Resume

所有操作都必须携带当前 `plan_id` 和 `revision`。`plan_action` 不能与 `text`、`images`、`plan_mode` 或 `execute_plan_id` 同时出现。批准、反馈和刷新都不会向历史追加合成 user 消息。反馈/回答会暂存在当前活动计划中，直到模型提交新 revision；若规划在此之前中断，`plan_state.plan.pending_feedback` 会返回该草稿供用户检查并重新提交。刷新提示仍只属于对应 Plan-only run。Runtime 会把精确 revision 和明确的 execute/resume 指令作为执行契约注入 Agent，并通过 `plan_state` 更新进度。`allow_stale` 只确认在本次过期证据快照下执行，不会刷新、重解释或替换批准 revision；明确的 goal、原步骤约束、verification、acceptance criteria 与 `completion_checks` 优先于宽泛 assumptions。全部进度项均为 `completed` 或明确 `skipped` 只是进入 `completed` 的必要条件；Finish 还必须通过该 revision 的服务端完成检查，否则保留真实进度/适应步骤，把绑定的原步骤置为 `blocked`，并进入可修订或 Resume 的 `failed`。History 的 `plans[]` 最多同步最近 50 个 revision，并始终包含当前 revision。LingClaw 启动时会把数据库中遗留的 `planning`/`executing` 状态恢复为 `stopped`；其中只有保留批准时间和执行次数的执行中断计划可以 Resume。

兼容旧客户端的 `{ "execute_plan_id": "plan_..." }` 仅可执行尚未修订的 revision 1 ready 计划；计划产生新 revision 后必须改用携带明确 revision 的 `plan_action`，避免旧页面批准未展示过的内容。新客户端始终应使用 `plan_action`。稳定错误 code 包括：

- `stale_plan_revision`：页面携带的 revision 已过期，或旧兼容入口尝试执行已修订计划；响应会在 `plan` 字段附带当前 `plan_state` 快照，客户端应立即同步后再操作
- `plan_not_ready`：当前状态不允许该操作
- `plan_already_active`：Session 已有活动计划或 run
- `group_plan_mode_unsupported`：Group 请求规划模式
- `plan_evidence_verification_failed`：批准或恢复前未能在限定时间内完成本地证据校验
- `plan_execution_incomplete`：Agent 结束执行时仍有未完成或未明确跳过的步骤；计划保留真实进度并进入 `failed`，可用 `resume` 继续
- `plan_completion_contract_failed`：步骤虽可能全部报告完成，但最终证据未满足批准 revision；错误事件携带绑定的 `plan_id`、`revision` 与失败 `checks[]`

### 5.2.5 只规划模式工具边界

`plan_mode: true` 的运行会调用大模型，但工具集合受限：

- 内置工具只暴露 `think`、`read_file`、`list_dir`、`search_files`、`http_fetch`、受限 `git_inspect`，以及满足图片模型/S3 条件的 `view_image`
- `git_inspect` 只接受 `status`、`diff`、`log`、`show`，不接受任意 Shell 参数
- MCP 工具只暴露当前 Session policy 已启用、`annotations.readOnlyHint=true` 且 `annotations.destructiveHint!=true` 的工具；缺少 annotations 时默认禁用。第三方 annotations 属于 LingClaw 信任的服务器声明
- 不暴露 `todos`、`exec`、`write_file`、`patch_file`、`delete_file`、`task`、`orchestrate`
- 如果模型仍尝试调用非只读工具，后端会拒绝该调用，不执行对应 handler
- Plan-only 必须通过内部 `submit_plan` 终结；`needs_input` 包含 1–5 个阻塞问题，`ready` 包含 1–12 个稳定 ID 步骤、至少一条验收标准，并用 `completion_checks` 覆盖每条 verification 与 acceptance criterion。结构总量上限 64KB，校验失败时模型可在同一 loop 内修正
- `completion_checks[].covers` 使用零基索引绑定 `verification` / `acceptance_criteria`；检查只能绑定原始步骤。`kind` 支持 `workspace_path`（最终路径类型及可选精确内容/字节数/SHA-256）、`approved_evidence_unchanged`（仅接受已捕获的 `file` 或非递归 `directory` 证据）、`plan_progress`、`tool_call_success`（工具名与实际执行参数必须精确一致）。其中 `plan_progress` 只增加附加进度门禁，不为任何条款提供服务器验收覆盖；每条 verification/acceptance 仍须至少由 `workspace_path`、`approved_evidence_unchanged` 或 `tool_call_success` 覆盖。同一路径的自相矛盾约束会在提交时拒绝
- 所有工作区文件工具、子进程 cwd、Plan 初始证据和 Finish 路径检查都以 no-follow 方式建立持久化工作区根 capability，并在实际打开、创建、删除、枚举、复核及读取期间保留同一信任锚；安全校验不会返回可供事后无 guard 裸路径重开的授权，也不会重新 canonicalize 根 pathname 后接受替换目标。Linux 与 Android 构建使用 `openat2` 的 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS | RESOLVE_NO_XDEV`，syscall/ABI 不可用时稳定 fail-closed。Windows 使用锁定父目录相对的 `NtCreateFile` 以零数据权限保留最终 identity，实际操作也从同一父链相对打开并复核目标；身份比较使用非零卷序列号与完整 128-bit `FILE_ID_INFO`，拒绝全零与全 FF 非唯一哨兵，查询不支持时 fail-closed。各操作按需申请最小权限且拒绝 reparse point；副作用操作通常持有不共享删除的祖先/目标句柄链，若已打开的 DELETE-access 句柄要求兼容路径，则保持 delete sharing，但会在每次操作前复核最初 identity。只能接收 cwd 或枚举 pathname 的系统 API 会在完整组件链 guard 存活时执行。stdio MCP 缓存会保留 workspace/cwd capability；Linux/Android 只让目标 MCP 子进程继承目录 fd（父进程副本保持 close-on-exec），Windows 在 root URI 使用期间保持 no-delete 句柄链。Streamable HTTP 只复核本地 capability，不声明或返回本地 roots；失配时逐出并关闭旧远端 Session。正式发布支持 Windows 与 Linux；其他 Unix 目标不会用 `st_dev` 冒充完整 mount identity，而是对安全工作区操作明确 fail-closed。文件检查在同一个受检句柄上读取 metadata、精确字节和 SHA-256，实际读取上限为 64 MiB。整个 verifier 使用较短的 `toolTimeout` 或 30 秒硬截止时间；`/stop`、run cancellation 或服务关闭会取消验证且不发送合同失败，截止时间耗尽则返回 `completion-contract-timeout` 检查并令计划安全进入 `failed`
- 不支持 Tool Calling 的 Provider 退化为单步骤 legacy plan，并保留原始 Markdown

### 5.2.6 忙碌期干预

当主 agent 正在运行时：

- 普通文本不会立刻开启新一轮，而是作为 deferred intervention 排队
- 带图干预只保留文本，图片会被丢弃
- `/stop` 会立即请求中止当前执行

### 5.2.7 Group chat 输入

连接到 `/ws?group=<id>&session=main` 后，客户端发送 group 消息：

```json
{
  "type": "group_message",
  "text": "请分别检查后端和前端风险",
  "targets": ["worker-a", "worker-b"],
  "target_mode": "selected",
  "start_runs": true,
  "run_mode": "execute"
}
```

字段说明：

- `target_mode`: `all` 使用 group 全部成员；`selected` 使用 `targets[]`；`mentions` 从消息中的 `@session-id` 提取，未命中时返回错误，不会回退成全员广播
- `start_runs`: `true` 时立即派发到目标 session；`false` 仅写入 group 消息
- `run_mode`: 当前只支持 `execute`；`plan_only` 返回稳定错误 `group_plan_mode_unsupported`
- 只有 `@session-id` 是派发协议；前端可把合法 token 显示为 `@Session Name`，但显示名本身不参与解析
- `@all` 需要 owner/admin 语义；当前浏览器 group UI 使用 `session=main` owner 连接，因此可广播。直接 `@session-id` 的成员必须回复；`@all` 覆盖但未直接点名的成员以可选回复派发，成员返回空或 `NO_REPLY` 时不会写入 group message
- 成员最终回复中的 `@session-id` 会触发一次后续派发；普通成员回复最多触发一个其他成员，避免无限互相唤起
- group chat 当前不支持图片附件
- group socket 支持普通非 slash 文本作为快捷 group message；结构化客户端应优先发送 `type:"group_message"` JSON

停止当前 group 内 queued/running 成员 run：

```json
{
  "type": "group_stop"
}
```

也可以传 `targets` 只停止指定成员：

```json
{
  "type": "group_stop",
  "targets": ["worker-a"]
}
```

派发是跨 session 异步并发、同一目标 session 串行排队。目标 session 使用自己的模型覆盖、MCP session policy 和 Skills；group 不提升任何权限。Group 只运行 Execute，因此 `enableTaskPlan` 仅按普通 Execute run 的兼容规则生效。每个目标 session 收到的用户消息格式固定为 group 上下文摘要加 main 指令，避免修改系统提示。

### 5.2.8 `session_control` 工具

`session_control` 是模型工具，只在 `main` session 的正常执行模式暴露；`plan_mode: true`、非 `main` session 和子代理都不会拿到该工具。后端执行层也会校验 `current_session_id == "main"`，即使模型或客户端伪造调用也会被拒绝。

支持动作：

- `list_sessions`: 返回轻量 session 名片；每行包含 `model`、`status` 和 `updated_at`，列表级别额外显示 `TaskPlan: enabled/disabled (global setting)`。为避免每次列表查询扫描所有 workspace，`agent` / `user` 摘要与 `skills` / `mcp_tools` 精确计数固定显示为 `unknown`；需要能力详情时使用 `describe_session`
- `create_session`: 创建一个新的持久化 session；后端生成随机 session id，可传入 `name`、`purpose`、`identity_profile`、`user_profile`、`style_profile`、`agent_notes` 初始化新 session 的 prompt 文件
- `delete_session`: 删除已存在的非 `main` Session；复用 `/delete` 安全约束，拒绝 `main`、当前 active 连接以及有 active/queued delegated work 的 Session。成功后先在 SQLite 事务中删除 Session 并清理 Group 成员/投票，再删除 Workspace 并广播列表
- `describe_session`: 按需查询单个 session 详情；参数为 `target`（兼容别名 `session_id`）、可选 `sections=["profile","capabilities","runtime","groups"]` 和 `max_chars`；未提供 `sections` 时默认返回 `profile`、`capabilities`、`runtime`
- `list_groups`: 返回 group 摘要
- `create_group`: 创建 group
- `update_group`: 更新 group 名称或成员
- `delete_group`: 删除 group；有 `queued/running` 成员 run 时拒绝
- `promote_group_admin`: 把 group 成员升级为 promoted admin；`main` 始终是隐式 owner
- `remove_group_member`: 移除 group 成员；省略 `requester_session_id` 或传 `main` 时直接移除，传 promoted admin id 时进入/追加 2/3 投票，达到阈值后自动移除
- `post_group_message`: 只向 group 历史写入 main 消息
- `dispatch`: 向 `targets[]` 或 `group_id` 成员派发任务，支持 `run_mode=execute`、`wait` 和 `summary_budget`；兼容字段传入 `plan_only` 时返回 `group_plan_mode_unsupported`
- `collect`: 汇总指定 group 的最近消息和 run 状态
- `stop`: 停止指定 targets 或 group 内 queued/running run

输入上限与兼容说明：

- `message` / group socket `group_message.text` 最多 32,000 字符
- `dispatch`/`stop` 的显式 `targets[]` 最多 16 个；group 广播可省略 `targets`，`all`/`mentions` 解析后的目标集合按 group members 上限最多 64 个
- `create_group`/`update_group` 的 `members[]` 最多 64 个
- `dispatch` 目标 session 必须已经存在；需要新 worker 时先调用 `create_session`，再使用 `list_sessions` 返回的精确 session id 派发
- session id 当前不做跨平台大小写归一；调用方应使用 `list_sessions` / group members 返回的精确大小写
- `target_mode="mentions"` 没有解析到合法 `@session-id` 时返回错误，不会回退成全员广播

`describe_session` 的分区语义：

- `profile`: 返回 `AGENTS.md`/`AGENT.md`、`IDENTITY.md`、`USER.md`、`SOUL.md` 的规则摘要；优先使用 frontmatter `summary`，其次提取结构化字段，再 fallback 到短段落摘要；模板未填写会标记 `template_unfilled=true`
- `capabilities`: 返回目标 session 当前模型、图片输入能力、内置工具名、启用的 Skills、当前 session policy 已启用且缓存可见的 MCP tools 以及只读/变更分类
- `runtime`: 返回 queued/running/idle 状态、直接派发 run、最近 group run 和最近失败工具调用 id 摘要
- `groups`: 返回该 session 所属或参与过的 group 摘要

`describe_session` 不返回 API key、MCP headers、环境变量、完整 system prompt 或完整 persona 文件。`list_sessions` 用于“选 session”，不读取每个 session 的 prompt/Skills/MCP workspace；`describe_session` 用于调度前确认单个 session 的详细能力，避免每次列表查询都消耗大量 token。

权限边界：

- `session_control` 只负责跨 session 调度，不改变目标 session 的工具权限、MCP policy、hooks 或 PlanOnly 边界
- `create_session` 只创建新 session，不修改已有 session 的身份文件；初始化文本只写入新 session workspace 的受控 prompt 文件
- `dispatch` 控制其他 session，不能把任务派发给 `main` 自己；目标包含 `main` 时会被后端拒绝，避免 main 等待自身 queued run 导致超时
- 目标 session 的 mutating 行为仍由其正常运行模式、工具权限和 hook 链决定
- group 默认单轮响应，被派发的 session 各自回复一次，不会自动触发无限互相回复

## 5.3 服务端 -> 客户端事件

下表列出前端需要处理的主要事件。

## 5.3.1 会话与历史

### `feature_status`

Settings 热保存功能开关后，普通 Session socket 收到：

```json
{
  "type": "feature_status",
  "features": {
    "groups": false
  }
}
```

Group 从开启变为关闭时，Runtime 先把活动成员 run 持久化为 `stopped`、取消成员任务并向相关连接广播一次该事件，然后关闭 Group socket。客户端应清空 Group 运行态并返回 Main；普通 Session socket 保持连接。重新开启后可重新请求原 Group 数据。

### `storage_status`

普通 Session 与 Group WebSocket 建立后都会收到当前存储状态；运行期首次进入保护模式时还会向所有连接广播一次：

```json
{
  "type": "storage_status",
  "storage": {
    "mode": "healthy"
  }
}
```

```json
{
  "type": "storage_status",
  "storage": {
    "mode": "protected",
    "code": "storage_protected"
  }
}
```

首次进入保护模式时，服务端先按精确 `(session_id, connection_id)` 删除每个被取消 direct run 的 `live_round`，再通过取消令牌终止活动 Agent/Group run。这样已释放 reservation 且不会再产生终态事件的 direct run，在重连时也不会重放伪 `start`；若同一 Session 已绑定更新的连接，其 replay 状态不会被旧 run 清理。该取消不设置用户 stop 标志、不触发 `/stop` hook，也不发送伪造的 `reason: user_stop`。随后普通消息、图片上传、Session/Group/Todo、Session Skills 和 MCP Session policy 等 SQLite 写操作被禁止。Session、历史、Usage 和配置读取继续可用；`.lingclaw.json` 是独立文件，仍可保存。服务端不通过协议暴露原始 SQL 错误；客户端应显示本地化修复提示并要求用户修复后重启。

### `session`

```json
{
  "type": "session",
  "id": "main",
  "name": "Main",
  "model": "openai/gpt-5.5",
  "effort": "medium",
  "explicitPrimaryModelConfigured": false,
  "modelOverridePresent": true,
  "modelOverrideConfigured": false,
  "effectiveModelConfigured": false,
  "configRevision": 1720684800124,
  "capabilities": {
    "image": true,
    "s3": true,
    "s3_config_id": "..."
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
- `model`: 当前 Session 的有效模型引用
- `effort`: 已按当前模型配置规范化的 Thinking Effort；非推理模型为 `off`
- `capabilities.s3`: 当前服务端是否可用 S3 上传能力
- `capabilities.s3_config_id`: 当前 S3 配置身份；S3 不可用时为 `null`。身份变化时客户端必须丢弃尚未发送的本地上传附件，远程 URL 附件不受影响
- `modelOverridePresent`: 当前 Session 是否持久化了 `/model` override。它只表示值存在，不表示该值仍能在当前 Config 中解析
- `modelOverrideConfigured`: 当前 Session 的持久化 `/model` override 是否仍能在当前运行时 Config 中成功规范化；删除对应 provider/model 后会变为 `false`，不包含全局配置或内置默认回退
- `explicitPrimaryModelConfigured`: 当前服务端运行时是否具有经过校验的显式全局主模型；配置保存后会向所有 Session 连接刷新该值
- `effectiveModelConfigured`: 当前 Session 最终是否允许启动 Agent run。若 Session 存在持久化 override，则只取决于该 override 是否仍有效；失效 override 不会静默回退到全局模型。前端发送门禁必须优先使用此字段
- `configRevision`: 生成上述模型状态字段时使用的配置修订号；同一个 payload 内的模型状态字段来自同一个不可变 Config 快照

### `session_model_configuration`

配置保存或任一 Session 成功更新模型/Effort 后，服务端向每个已连接 Session 发送最小模型状态事件：

```json
{
  "type": "session_model_configuration",
  "id": "main",
  "model": "openai/gpt-5.5",
  "effort": "high",
  "explicitPrimaryModelConfigured": true,
  "modelOverridePresent": false,
  "modelOverrideConfigured": false,
  "effectiveModelConfigured": true,
  "configRevision": 1720684800125,
  "capabilities": {
    "image": true,
    "s3": true,
    "s3_config_id": "..."
  }
}
```

该事件只更新模型门禁和随模型/配置变化的能力，不携带或覆盖 Session `name`、Usage、历史、Todos 等独立状态。首次连接和真正的 Session 切换仍使用完整 `session` 事件。

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
      "message_index": 1,
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
      "message_index": 2,
      "thinking": "...",
      "run_outcomes": [
        {
          "session_id": "main",
          "run_id": "run-18d077d8a3eec370-1",
          "run_connection_id": "42",
          "status": "completed",
          "phase": "finish",
          "reason": "complete",
          "duration_ms": 840,
          "start_message_index": 1,
          "end_message_index": 2,
          "plan_id": null,
          "plan_revision": null,
          "started_at": 1710000000,
          "finished_at": 1710000001
        }
      ]
    },
    {
      "role": "tool_call",
      "name": "read_file",
      "arguments": "{\"path\":\"README.md\"}",
      "id": "call_123",
      "message_index": 3
    },
    {
      "role": "tool_result",
      "result": "file content ...",
      "id": "call_123",
      "is_error": false,
      "message_index": 4,
      "images": [
        {
          "url": "https://...fresh-signed-url...",
          "name": "screenshot.png",
          "mime_type": "image/png"
        }
      ],
      "subagent_snapshot": {
        "tools": [
          {
            "id": "sub-call-1",
            "name": "view_image",
            "result": "Attached 1 validated image.",
            "is_error": false,
            "images": [
              {
                "url": "https://...fresh-signed-url...",
                "name": "diagram.jpg",
                "mime_type": "image/jpeg"
              }
            ]
          }
        ],
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

History 顶层还可包含结构化计划历史：

```json
{
  "plans": [
    {
      "plan_id": "plan_...",
      "revision": 1,
      "historical": true,
      "status": "ready",
      "message_index": 12,
      "created_at": 1710000000,
      "updated_at": 1710000010,
      "artifact": {
        "title": "更新存储层",
        "goal": "完成迁移并保持兼容",
        "summary": "先核验 schema，再实现并验证。",
        "steps": [
          { "id": "inspect", "title": "核验现状", "description": "检查 schema 与调用方" }
        ],
        "assumptions": [],
        "risks": [],
        "verification": ["运行完整测试"],
        "acceptance_criteria": ["迁移可回滚"],
        "completion_checks": [
          {
            "id": "storage-tests",
            "step_id": "inspect",
            "covers": [
              { "section": "verification", "index": 0 },
              { "section": "acceptance_criteria", "index": 0 }
            ],
            "kind": "tool_call_success",
            "tool_name": "exec",
            "arguments": { "command": "cargo test storage" }
          }
        ],
        "questions": []
      },
      "progress": [
        { "id": "inspect", "title": "核验现状", "status": "pending", "note": "" }
      ]
    }
  ],
  "pending_plan": {
    "plan_id": "plan_...",
    "revision": 2,
    "message_index": 14,
    "created_at": 1710000000
  }
}
```

- `plans[]` 按计划消息位置与 revision 排序；旧 revision 带 `historical:true`，客户端必须只读折叠展示。当前 revision 带 `historical:false`
- `status` 取值为 `planning`、`needs_input`、`ready`、`executing`、`completed`、`failed`、`stopped`、`discarded`
- `discarded` 是服务器权威终态：客户端收到后应按当前 Session、已验证 run 和精确 `plan_id + revision` 找到关联 execution stack，包括 done 后仍挂载的已结束栈，并收口为中性结果，移除问题表单、Answer/Resume/Revise 恢复入口，只保留只读 Plan 与复制能力；点击“丢弃”本身不能乐观伪造该状态
- `progress[].status` 取值为 `pending`、`in_progress`、`completed`、`blocked`、`skipped`；适应性步骤可能只存在于 `progress[]`，并携带 `deviation_reason`
- `artifact.completion_checks[]` 是批准 revision 的不可变、可展示合同；空字段可能因 `skip_serializing_if` 省略。新结构化 ready revision 必须用非 `plan_progress` 的服务器检查完整覆盖 verification/acceptance；progress check 只能作为额外门禁。旧持久化结构若包含验收/验证但缺少检查，或只有 progress 自报检查，不能只凭自由文本 note 完成，需修订后重试
- `unfinished_steps` 表示尚未被 Agent 报告为 `completed` 或 `skipped` 的步骤；服务端不会自动补成 completed。当前执行若带着未完成步骤或失败完成检查结束，计划进入 `failed`；`run_finished_with_unreported_steps` 仅用于兼容展示旧版本已经持久化的异常 `completed` 记录
- `pending_plan` 仅作为旧客户端兼容入口，在当前计划为 `ready` 时出现；新客户端以 `plans[]` 为准

补充说明：

- `todos` 工具的 `tool_call` / `tool_result` 不会进入这里的可见历史列表
- 可见 `user`、`assistant`、`tool_call` 和 `tool_result` 项都携带原始 `message_index`；`plans[].message_index` 用它定位对应 assistant 计划消息
- 同一存储消息可展开为多个可见项，例如中间 assistant 文本及其 `tool_call` 具有相同 `message_index`。客户端应按 `run_outcomes[]` 的闭区间及其所在的最后可见项划分 execution stack，不得把中间文本或区间内用户补充指令当成终点；初次 History、重连和加载更早消息使用同一划分，分页边界需扩展到完整运行起点。后续 Plan revision 若复用原始 user anchor，不得重新认领前一终态已经覆盖的可见项。缺失终态或尾部截断的过程仍降级为 incomplete；无步骤成功不创建空栈
- SQLite v7 以稳定 `run_id` 保存顶层运行终态，并将每条事实附在其不可变消息边界内最后一个可见 History 项的 `run_outcomes[]`。`status` 为 `completed`、`failed`、`blocked`、`waiting_user`、`partial`、`stopped` 或 `incomplete`；同一事实还携带 phase/reason/duration、连接身份、边界、可选 Plan 身份与时间。准确 run 的生产协程在 gate 外只冻结其消息尾、精确预期/替换 Plan、Sub-agent 快照和失败工具标记；最终按 `Session persist gate -> sessions lock -> 合并最新 Session -> 释放 sessions lock -> SQLite transaction -> 释放 gate` 提交 outcome、合并后的 Session 与最终 Plan。先提交的 Todos/revision、模型/Effort、Usage、工作目录绑定和其他非 run 字段必须保留；消息尾或 Plan 代际变化则 fail-closed。Plan 完成/失败不会先由独立保存或事件广播暴露。状态机仅允许在 `PreCommit` 做最后一次精确 Stop 仲裁；进入 `CommitChosen` 后必须等待不可撤销的 `tokio-rusqlite` 操作返回明确结果，成功后才在同一代际上字段级更新内存并广播缓存的 hook/Plan/terminal 事件。异步 live dispatcher 不会重新读取更晚的 Session。读取会在同一次数据库 read 中取得并验证真实连续消息数，负数、反向、空 transcript 或超出实际消息数的边界均视为存储损坏并进入保护模式。客户端必须以该事实恢复重启/重连后的 execution stack；只有旧数据库或确实缺失事实的过程才安全降级为 `incomplete`。保存逐项验证每条旧事实原消息区间的指纹与位置；只删除区间内实际发生修改、删除或移位的事实。区间外 system 时间/Goal/Observation 更新、无关消息编辑与普通 append 会保留该事实，不能猜测重新绑定到不同消息
- `start_message_index` 来自 reservation 时捕获的精确 user-message 指纹锚点，而不是缓存数组下标。BeforeAnalyze 自动压缩、前缀裁剪或签名 URL 规范化后，服务端必须重新解析同一锚点；缺失或无法区分的重复锚点会安全拒绝终态提交。PlanOnly 刷新/反馈若在同一终态事务注册新 revision，`plan_id` / `plan_revision` 必须取最终 replacement Plan
- 后台 Memory/Reflection 的 Usage 在 Provider 成功后按唯一 operation id 以增量方式提交；幂等标记、总量、日量和 Provider/role label 位于同一 SQLite 事务，并与顶层终态共用 Session persist gate。Memory 的提交发生在提取 JSON 解析、merge 和私有文件保存之前，因此 Provider 已成功后的无效 JSON 或保存失败仍保留 Usage。Session 未加载到内存不影响持久提交；SQLite Session 已被并发删除时返回正常 `Missing` 领域结果，不进入 storage protection。App-owned auxiliary registry 把任务绑定到 canonical Session allocation 与功能 enable cycle；Session 删除先关闭注册并 drain 精确 allocation，再获取 persist gate，功能热禁用、storage protection 与 graceful shutdown 也会取消并等待注册任务。私有 Memory/Reflection/audit 写入还需在注册表内取得 operation-scoped 写许可：取消先赢时不启动写入，写入先赢时 teardown 会等待任务退出。Session ready/recreate 与 connection binding 使用同一个 canonical control lock，删除取得 persist gate 后再次核对 exact closed lifetime 和活动 connection/run，并持锁至私有 Home 清理完成；并发重连不会留下已删除 Session 的幽灵连接或旧 allocation 任务
- 顶层 `tool_result.images` 以及 `subagent_snapshot.tools[].images` 为可选字段；每项只包含新鲜签名的 `url`、展示 `name` 和经过校验的 `mime_type`，不会暴露 object key、S3 配置身份或 Base64。若历史图片所属的 S3 配置身份已失效，该图片会被跳过，文本结果仍正常回放
- 前端应使用 `todos_state` 渲染 todo 面板，而不是从 `history.messages` 反推

## 5.3.2 一轮主执行中的基础事件

### `start`

表示一轮回复开始。

```json
{
  "type": "start",
  "run_id": "run-18d077d8a3eec370-1",
  "run_connection_id": "42",
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
- `run_id` 是服务器为一次顶层 reservation 生成的稳定 opaque 身份，同一 run 的多个 ReAct cycle、live replay 与终态保持不变；它也作为 SQLite terminal outcome 的主身份。客户端不能自行生成或用文本内容推断
- `run_connection_id` 是服务器分配的 opaque 字符串，标识拥有该顶层 run 的 WebSocket connection epoch；实时 `start` 与重连生成的 replay `start` 使用相同值。客户端必须把它与本地新建的 client-run 序列共同绑定，不能让任意当前 run 认领旧连接的终态
- 仅当协商结果明确为 legacy（`protocols.execution_identity` 缺失），客户端才可为首个 socket generation 上缺少该字段的 `start` 合成连接内身份；严格协议下缺失字段的 `start` 是可见的协议错误，不能进入 busy
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

### `task_plan`

启用 `settings.enableTaskPlan`（界面名称“自动执行提纲”）后，没有批准计划的普通 Execute run 会发送临时规则提纲。该事件由规则生成，不调用 LLM；输入包括当前用户请求、运行期 `WorkingState`、任务记忆、最近工具结果、已发现子代理以及当前 Session policy 允许的内置/MCP 工具。`task_plan` 进入 live replay，但不会写入 Session messages，也不会自动执行验证命令。Plan-only 与批准计划执行期间完全抑制该机制，避免与用户批准的结构化计划形成第二套计划。

```json
{
  "type": "task_plan",
  "round": 3,
  "cycle": 1,
  "plan": {
    "goal": "Fix MCP timeout handling",
    "intent": "change",
    "steps": [
      {
        "id": "inspect",
        "title": "Inspect relevant code",
        "status": "pending"
      }
    ],
    "openQuestions": [],
    "suggestedTools": [
      {
        "name": "read_file",
        "reason": "Inspect current implementation before editing",
        "score": 5,
        "source": "intent"
      }
    ],
    "suggestedAgents": [],
    "verificationSuggestions": [
      {
        "command": "cargo test mcp",
        "reason": "MCP behavior appears relevant",
        "confidence": "high",
        "when": "before_finish"
      }
    ],
    "acceptanceCriteria": ["Relevant tests pass"],
    "status": "active"
  }
}
```

字段说明：

- `intent` 取值为 `inform`、`change`、`investigate` 或 `execute`
- `steps[].status` 是运行期软状态，例如 `pending`、`done`、`ready`
- `suggestedTools[].source` 可为 `query`、`memory`、`plan`、`recent_failure`、`intent`；MCP 工具只会来自当前 session policy 已启用集合
- `verificationSuggestions[]` 只表示建议模型在合适时机选择执行，runtime 不会自动运行、弹确认或改变工具权限模型
- `status` 为当前计划状态，通常为 `active` 或 `ready`；收到 `done` 后前端可将面板标记为 complete/stale

### `plan_state`

计划创建、提问、修订、批准、步骤更新和终态变化都会发送完整快照：

```json
{
  "type": "plan_state",
  "plan": {
    "plan_id": "plan_...",
    "revision": 2,
    "status": "executing",
    "message_index": 14,
    "created_at": 1710000000,
    "updated_at": 1710000100,
    "approved_at": 1710000090,
    "execution_attempt": 1,
    "artifact": {
      "title": "更新存储层",
      "goal": "完成迁移并保持兼容",
      "summary": "按已确认步骤执行。",
      "steps": [{ "id": "inspect", "title": "核验现状", "description": "" }],
      "assumptions": [],
      "risks": [],
      "verification": ["运行完整测试"],
      "acceptance_criteria": ["迁移可回滚"],
      "completion_checks": [
        {
          "id": "storage-tests",
          "step_id": "inspect",
          "covers": [
            { "section": "verification", "index": 0 },
            { "section": "acceptance_criteria", "index": 0 }
          ],
          "kind": "tool_call_success",
          "tool_name": "exec",
          "arguments": { "command": "cargo test storage" }
        }
      ],
      "questions": []
    },
    "progress": [
      { "id": "inspect", "title": "核验现状", "status": "in_progress", "note": "正在检查" }
    ],
    "evidence_count": 3,
    "evidence_truncated": false,
    "stale_override_paths": [],
    "stale_override_confirmed_at": null,
    "initial_submission_pending": false,
    "initial_request_image_only": false,
    "unfinished_steps": 1,
    "run_finished_with_unreported_steps": false
  }
}
```

`initial_submission_pending=true` 表示模型尚未通过 `submit_plan` 交付首个计划 revision，客户端应把 artifact 中的初始占位标题视为内部数据并使用本地化状态文案展示。`initial_request_image_only=true` 进一步表示该初始请求只有图片输入，客户端可显示本地化的图片规划提示。两个字段在已提交的正式 revision 中均为 `false`。Artifact 中使用 `skip_serializing_if` 的空数组字段（例如初始或提问阶段的 `steps`）可能省略，客户端应按空数组处理。

### `plan_stale`

执行或恢复前发现本地证据变化时发送，且不会启动 run：

```json
{
  "type": "plan_stale",
  "code": "plan_stale",
  "plan_id": "plan_...",
  "revision": 2,
  "paths": ["src/main.rs", "frontend/src"],
  "evidence_incomplete": false,
  "confirmation_token": "8f4b..."
}
```

客户端应让用户选择 `refresh`，或再次发送 `execute/resume`，并同时设置 `allow_stale:true` 与原样回传 `stale_confirmation_token`。确认令牌绑定当前 Plan revision 和本次实际读取到的证据快照；缺少令牌、令牌不匹配，或警告后证据再次变化时，Runtime 会返回新的 `plan_stale`，不会开始执行。确认成功后，Runtime 会在 `stale_override_paths` 记录被覆盖的变化路径，并在 `stale_override_confirmed_at` 持久化本次明确确认的秒级 Unix 时间；即使 `paths` 为空，该时间仍会记录。`evidence_incomplete=true` 表示部分本地证据因采集错误或数量上限未能完整记录，此时 `paths` 可以为空，但仍必须刷新或明确覆盖。本地证据最多记录 256 项；文件/目录使用工作区相对路径与内容指纹，受限 `git_inspect` 使用原查询参数与结果指纹，从而覆盖与该查询相关的工作树、索引和提交变化。MCP/HTTP 外部数据不进入可重新验证路径列表。

### `plan_ready`（兼容事件）

结构化计划进入 `ready` 后仍发送该事件供旧客户端使用。新客户端应使用 `plan_state` 与 History `plans[]`。

```json
{
  "type": "plan_ready",
  "plan_id": "plan_...",
  "revision": 2,
  "message_index": 12,
  "created_at": 1710000000
}
```

字段说明：

- `plan_id`: 当前 Session 内待执行计划 id
- `revision`: 当前可执行 revision
- `message_index`: assistant 计划消息在 session messages 中的位置
- `created_at`: 创建时间戳，秒级 Unix time

### Group events

`/ws?group=<id>&session=main` 使用以下事件恢复和更新 group UI。

```json
{
  "type": "group",
  "id": "a1b2c3",
  "name": "Review Group",
  "members": ["worker-a", "worker-b"],
  "explicitPrimaryModelConfigured": false,
  "model_override_members": ["worker-a"],
  "model_configured_members": ["worker-a"],
  "configRevision": 1720684800124,
  "capabilities": {
    "s3": true,
    "s3_config_id": "..."
  },
  "admins": ["worker-b"],
  "pending_votes": [],
  "member_details": [
    { "id": "main", "name": "Main", "role": "owner" },
    { "id": "worker-a", "name": "Worker A", "role": "member" },
    { "id": "worker-b", "name": "Worker B", "role": "admin" }
  ],
  "created_at": 1710000000,
  "updated_at": 1710000300
}
```

```json
{
  "type": "group_history",
  "group_id": "a1b2c3",
  "members": ["worker-a", "worker-b"],
  "explicitPrimaryModelConfigured": false,
  "model_override_members": ["worker-a"],
  "model_configured_members": ["worker-a"],
  "configRevision": 1720684800124,
  "admins": ["worker-b"],
  "pending_votes": [],
  "member_details": [
    { "id": "main", "name": "Main", "role": "owner" },
    { "id": "worker-a", "name": "Worker A", "role": "member" },
    { "id": "worker-b", "name": "Worker B", "role": "admin" }
  ],
  "messages": [],
  "runs": []
}
```

配置保存或任一 Session 成功更新模型/Effort（包括 `/model`、`/think` 与 `PUT /api/session-models`）后，Group 连接收到只含模型状态的专用事件：

```json
{
  "type": "group_model_configuration",
  "id": "a1b2c3",
  "model_member_ids": ["worker-a", "worker-b"],
  "explicitPrimaryModelConfigured": true,
  "model_override_members": ["worker-a"],
  "model_configured_members": ["worker-a", "worker-b"],
  "configRevision": 1720684800125,
  "capabilities": {
    "s3": true,
    "s3_config_id": "..."
  }
}
```

`group_model_configuration` 不携带或覆盖 Group `name`、成员、管理员、投票、消息或运行历史。`model_member_ids` 是生成模型状态时使用的成员集合，仅用于前端确认该快照仍对应当前 roster；不一致时前端必须先让旧模型状态失效，再只刷新当前 roster 的模型配置，不能用该字段回滚成员列表。`capabilities.s3` 与 `capabilities.s3_config_id` 是同一 Config 修订下的全局上传状态，使停留在 Group 页面中的客户端也能立即丢弃旧存储身份的待发送附件；Group payload 不推断多成员共同的图片模型能力。

`model_override_members` 只列出持久化 Session `/model` override 在当前 Config 中仍然有效的成员，保留用于诊断和兼容。`model_configured_members` 列出最终允许启动 Agent run 的成员：无 override 的成员可使用经过校验的全局模型；存在 override 的成员必须保证 override 仍有效，失效 override 不会回退到全局模型。前端 Group 门禁必须直接使用 `model_configured_members`，服务端也会按相同语义拒绝包含未配置目标的 dispatch。

`explicitPrimaryModelConfigured` 与 Session payload 中同名字段语义相同。`configRevision` 与 Session payload 使用同一修订序列；同一个 Group payload 内的全局和成员模型状态都基于该修订号对应的 Config 快照。成员成功更新模型或 Effort 后，所有已连接 Session 和 Group 都会收到相同新序号的状态 payload，避免全局序列推进后未关联页面永久保留旧序号。配置保存广播同样会在一个不可变 Config 快照下生成全部 Session/Group 模型字段，并与 Session 模型偏好广播及其他配置保存串行，避免混合新旧状态。

每个 Agent run 会在取得 reservation 后、写入目标消息前获取经过校验的 Config/Session 模型快照，并在整个 run 内复用该快照；task/orchestrate 未配置专用子代理模型时继承该快照中的 Session 模型。因此普通消息、busy intervention rerun、直接 `session_control.dispatch`、成员回复触发的后续 `@session-id` 派发，以及排队期间发生的配置热重载都不能落入内置默认模型；`/new` 压缩也在实际命令入口使用同样的快照规则。自动 mention 后续派发若缺少有效模型，会生成可见的 failed group run，而不是只写服务端日志。

```json
{
  "type": "group_message",
  "group_id": "a1b2c3",
  "message": {
    "id": "gmsg_...",
    "role": "session",
    "session_id": "worker-a",
    "content": "检查结果摘要",
    "timestamp": 1710000400,
    "run_id": "grun_..."
  }
}
```

```json
{
  "type": "group_run_started",
  "group_id": "a1b2c3",
  "run": {
    "id": "grun_...",
    "group_id": "a1b2c3",
    "session_id": "worker-a",
    "status": "queued",
    "prompt": "请检查后端风险",
    "created_at": 1710000400,
    "updated_at": 1710000400
  }
}
```

```json
{
  "type": "group_member_event",
  "group_id": "a1b2c3",
  "run_id": "grun_...",
  "session_id": "worker-a",
  "event": {
    "type": "tool_call",
    "name": "read_file",
    "id": "call_..."
  }
}
```

```json
{
  "type": "group_member_status",
  "group_id": "a1b2c3",
  "run_id": "grun_...",
  "session_id": "worker-a",
  "status": "completed",
  "result_excerpt": "检查结果摘要",
  "error": null,
  "updated_at": 1710000500
}
```

```json
{
  "type": "group_run_completed",
  "group_id": "a1b2c3",
  "run_id": "grun_...",
  "session_id": "worker-a",
  "status": "completed",
  "result_excerpt": "检查结果摘要",
  "error": null,
  "completed_at": 1710000500,
  "updated_at": 1710000500
}
```

说明：

- `group_message.role` 可为 `user`、`main`、`session`、`system`
- `group_member_event.event` 是目标 session 原 live event 的包装；目标 session 自身 live replay 也会保留这些事件
- 当前前端群聊默认隐藏 `group_run_started`、普通 `group_member_status` 和成员 live 过程卡片，只渲染错误、管理/投票 system 消息和最终 `role=session` 回复；客户端仍可消费这些事件构建更详细的运行视图
- 正常完成或失败但已有成员输出时，成员最终摘要会先作为 `group_message` 写入 group 历史；`group_run_completed` 用于状态收敛，前端不需要再把 `result_excerpt` 渲染成第二条消息
- `status` 可为 `queued`、`running`、`completed`、`failed`、`stopped`

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
  "name": "view_image",
  "result": "Attached 1 validated image.",
  "duration_ms": 120,
  "is_error": false,
  "images": [
    {
      "url": "https://...signed-url...",
      "name": "screenshot.png",
      "mime_type": "image/png"
    }
  ]
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
- `images` 可选且仅在至少一张工具图片成功校验并上传时出现；格式与历史中的工具图片一致。图片失败说明会追加到 `result`，但不会改变原工具的 `is_error`

### `tool_image_compatibility_warning`

OpenAI-compatible Chat 端点在流开始前明确拒绝图片/tool 内容组合时，Runtime 会移除本轮工具图片并自动重试一次，同时发送一次本地化警告事件：

```json
{
  "type": "tool_image_compatibility_warning",
  "provider": "openai_chat"
}
```

该事件每个 Agent run 最多发送一次。重试后，本次 run 的后续 cycle 不再附加工具图片；鉴权、限流和普通 schema 错误不会触发该降级。

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
  "run_id": "run-18d077d8a3eec370-1",
  "run_connection_id": "42",
  "phase": "finish",
  "reason": "complete",
  "duration_ms": 840,
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

`done` 并非所有失败运行都必然发送：请求预算超限、Provider/Analyze 失败等 `run_failed` 路径会以带 `run_terminal: true` 的 `error` 独立结束本轮，之后可能再发送匹配的 `plan_state(failed)`，但不会补发 `done`。顶层 `done` 与终态 `error` 都携带和对应 `start` 相同的 `run_id + run_connection_id`；`duration_ms` 是服务端运行事实，缺失时由同一 run 的服务器开始时间计算，而不是由客户端猜测。客户端只在该服务器身份和本地 client-run 序列均匹配时收口活动 run，同时保留按 server/client run 与 Plan ID 关联的终态视图，以便同一运行的迟到 `plan_state` 或兼容性 `done` 原位更新。若精确匹配的 attention `done` 到达前没有 Tool/Reasoning/ReAct/Plan 步骤，客户端仍创建唯一、身份化的终态栈并呈现摘要、ARIA 和恢复入口；`finish/complete` 的无步骤成功运行不创建空噪音栈。若终态 user-message 指纹锚点缺失或歧义，服务端发送一次 `phase=incomplete` / `code=terminal_identity_unavailable` 的终态 error，不发送 `done`，也不猜写 SQLite outcome；客户端收口精确 live stack，但后续 History 安全显示 incomplete。若重连或 History/Session reset 已丢弃该关联，无身份或身份不匹配的迟到 `done` 不得创建/修改 execution stack、清除 busy 或结束新的 run；其中的全局 Usage 总数仍可独立应用。仅明确协商的 legacy 首连接可用 socket-generation 合成身份收口无字段终态，该连接断开后不再兼容。严格协议中，当前活动 run 的无身份 `done`/终态 `error` 会关闭该准确连接并保持 fail-closed，而没有活动/待确认 run 的迟到无身份终态只会被忽略。`run_terminal: false` 或缺失字段均为非终态，新 run、Session/History 切换或重连不得复用旧关联。服务器在终态生产点、释放 run reservation 与处理待重跑输入前，以精确 `run_id` 冻结 outcome 与 run-owned patch；取得唯一 persist gate 后才从内存读取最新 Session 并字段级合并，在一个 SQLite transaction 中不可取消地等待所选提交结果。成功确认后才按同一代际更新内存并依次发送缓存的 hook/Plan 事件与终态，再交给异步 dispatcher 转发。写入错误进入现有 storage-protected 路径，且不会假定已排队的 SQLite 事务可由丢弃 async future 撤销。

补充说明：

- 普通执行尚有未恢复的工具或委派失败时，发送并持久化 `done phase=partial reason=unresolved_tool_failures`。相同规范调用的成功重试会恢复旧失败；读取同一文件时允许修正行范围，搜索内容、命令、子任务输入及其他动作参数不得被忽略。失败详情仍保留，partial 不自动折叠；只有完全恢复才允许 `finish/complete`。
- PlanOnly 提交阻塞问题时，终态为 `waiting_user/needs_input`。随后 Discard 的权威 Plan 状态会中性收口相同 Session/run/Plan revision 的栈，清除恢复文本并保留原耗时；旧 revision 或更旧更新不能回退现态。
- `hard_cap` 等 incomplete/partial/blocked 异常出口会把相同代际的 planning/executing Plan 更新为 failed，并与 run outcome 原子保存后才广播；已报告进度、检查和批准信息保留。执行中断可 Resume/Revise/Discard，未批准规划可 Revise/Discard。数据库失败或 Plan 代际不符不会提前发布失败 Plan。

- 用户主动停止时，可能是：

```json
{
  "type": "done",
  "run_connection_id": "42",
  "phase": "stopped",
  "reason": "user_stop"
}
```

- 已批准计划在仍有未完成步骤时结束，会先发送 `plan_execution_incomplete` 错误，再以失败终态结束：

```json
{
  "type": "done",
  "run_connection_id": "42",
  "phase": "failed",
  "reason": "incomplete_plan"
}
```

- 全部步骤虽已报告完成，但最终证据不满足批准 revision 时，先发送绑定合同的错误，再以失败终态结束。自由文本 `note` 或适应步骤不能覆盖该结果：

```json
{
  "type": "error",
  "run_terminal": true,
  "run_connection_id": "42",
  "code": "plan_completion_contract_failed",
  "plan_id": "plan_...",
  "revision": 2,
  "checks": [
    {
      "check_id": "result-bytes",
      "step_id": "verify-result",
      "reason": "file size is 18 bytes; the approved contract requires 17 bytes"
    }
  ],
  "content": "The final workspace or execution evidence did not satisfy the immutable approved revision. The plan was marked failed and can be revised or resumed.",
  "dismissible": true
}
```

```json
{
  "type": "done",
  "run_connection_id": "42",
  "phase": "failed",
  "reason": "completion_contract_failed"
}
```

若 Finish 验证或其后的 `OnFinish` hook、Usage 聚合、persist gate/run-owned patch 准备在 `PreCommit` 因 `/stop`、连接/run cancellation 或服务关闭被取消，不会发送上述错误、Plan/hook 事件或自然 `done`，也不会排入 Memory/Reflection；正常的外层运行终止流程在唯一事务中原子持久化 stopped Plan、Session 与 outcome。数据库任务入队前会再做一次精确 Stop 仲裁；一旦进入 `CommitChosen`，该 SQLite future 不再参与 cancellation `select!`，提交成功的自然终态保持唯一权威事实，随后到达的 Stop 不会另写 `stopped`。若验证超过硬截止时间，`checks[]` 使用稳定的 `check_id: "completion-contract-timeout"` 和非敏感原因，并按合同失败结束。

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

`task_id` 是独立的随机委派身份；可选 `parent_tool_call_id` 是发起该 task 的真实顶层工具调用 ID，客户端不能用 Agent 名称或事件顺序猜测关联。正常启动与根据子工具输出补发的启动事件都保留该关系，live/replay/history 按同一父调用计数一次，原工具检查器与委派详情仍可查看。没有可信父 ID 的旧事件保持兼容，但不猜测去重。Orchestration 继续使用其既有父调用字段。

主代理通过 `task` 工具发起子代理任务。

```json
{
  "type": "task_started",
  "task_id": "task-1",
  "parent_tool_call_id": "call-1",
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
  "parent_tool_call_id": "call_abc123",
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

`parent_tool_call_id` 可选，绑定顶层 `tool_call.id`。客户端据此将工具检查器与编排面板视为一次调用，进度按真实子任务计数；旧服务端缺失该字段时不得猜测绑定。

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
  "run_terminal": false,
  "content": "..."
}
```

`run_terminal` 是服务器权威的顶层运行作用域标记：只有严格等于 `true`，并且终态 `error` 的 `run_connection_id` 与当前 client-run 绑定的服务器身份匹配时，`error` 才是可独立结束该 run 的终态事实，并解除 busy/ReAct/timer 状态。终态错误由服务端自动加入该身份；非终态错误不需要它。仅协商为 legacy 的首个 socket generation 可把无字段终态绑定到该连接先前的无字段 `start`；断线、History/Session reset 或第二连接后，这条兼容路径失效。`false` 或缺失字段必须 fail-safe 为非终态；busy `/think`、command hook、Plan action、Session/模型/工作区预检等错误只展示错误卡，活动运行及其 `live_round` 继续。空闲状态错误同样不得创建或复用 execution stack。Storage protection 通过独立 `storage_status` 事件收口精确匹配的活动 direct run；Plan action、Session/Group transition 或 Group-only busy 不属于顶层 direct run，不得生成执行栈。

真实顶层运行的 Provider、请求构造或上下文预算失败可携带 `diagnostic: {"code":"..."}`。该字段只有封闭类别，没有上游正文、URL、header 或凭据；`content` 使用固定安全文案，客户端按类别本地化。持久化时类别码写入同一 run outcome 的 `reason`，History 重建相同的 `diagnostic`；不新增 SQLite 列或改变 schema v7。类别/原因不一致或非 failed 事实携带诊断会被拒绝。旧记录没有可识别类别时省略该字段并安全降级。

分类保留错误来源：只有本地请求发送层生成的真实 HTTP 状态、连接或请求构造错误可使用对应 transport 类别。Provider 的 JSON/SSE 错误字段先由适配器加固定协议封装，包括 Responses 根级 `message`、嵌套 error 与 incomplete detail；HTTP200 中的上游自由文本即使以 `API 401`、`HTTP error:` 或 `Provider configuration error:` 开头，也仍按上游响应失败处理，不能触发本地 transport 诊断或重试/能力降级。历史已存的错误码按原事实读取，不重新扫描旧正文猜测来源。

| `diagnostic.code` | 含义 | 恢复内容 |
|---|---|---|
| `provider_authentication` | HTTP 401/403 | 检查 Models 中的凭据与权限 |
| `provider_rate_limited` | HTTP 429 | 等待后重试，检查服务限额 |
| `provider_unavailable` | HTTP 408/5xx | 稍后重试或换用已配置模型 |
| `provider_connection` | 本地 transport 连接失败 | 检查网络及 Models 中的地址 |
| `provider_request_rejected` | 其他 HTTP 4xx | 检查模型、协议和请求能力 |
| `provider_response_invalid` | 无法使用的 Provider 响应 | 检查模型和协议 |
| `model_configuration` | 本地 request builder 拒绝配置 | 检查 Provider 地址或凭据 |
| `context_budget_exceeded` | 真实运行的输入预算不足 | 减少上下文/推理强度或选择更大窗口模型 |

客户端“查看错误”打开当前精确 run 的安全诊断详情，合适时提供 Models 入口；不能复制成多条错误卡，也不能因为普通 Composer 配置预检失败而创建不存在的 run、消息锚点或 outcome。终态身份、事务、Stop 仲裁及 Plan 代际规则保持不变。

终态消息锚点无法安全解析时使用以下 live-only 错误；它只负责收口准确客户端运行，不能据此生成持久 outcome：

```json
{
  "type": "error",
  "run_terminal": true,
  "run_id": "run-18d077d8a3eec370-1",
  "run_connection_id": "42",
  "phase": "incomplete",
  "reason": "terminal_identity_unavailable",
  "code": "terminal_identity_unavailable",
  "content": "LingClaw could not safely bind this run's final state to its originating message...",
  "dismissible": true,
  "recoverable": true
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

Agent 级瞬态 LLM 重试使用结构化变体，而不是带完整 Provider 错误正文的永久 system 消息：

```json
{
  "type": "progress",
  "kind": "llm_retry",
  "attempt": 2,
  "max_attempts": 2
}
```

客户端应在当前运行栈中短暂显示 attempt；收到 token/Tool 或终态后清除。若最终仍失败，带 `run_terminal:true` 的错误/执行栈是唯一持久可见错误正文，不能再复制 retry notice。

## 6. 图片输入协议细节

### 6.1 受信任上传图片

上传成功后，前端应优先回传：

```json
{
  "url": "https://...",
  "object_key": "lingclaw/images/...",
  "attachment_token": "...",
  "s3_config_id": "..."
}
```

这样服务端会：

- 校验 `attachment_token`，且签名绑定完整 S3 配置身份
- 确认 `s3_config_id` 仍等于服务端当前配置身份
- 重新生成可信 URL
- 避免客户端伪造任意 S3 object key

新接收的上传会把 `s3_config_id` 与 object key 一起持久化。配置轮换后，旧附件不会使用新 endpoint/bucket 重新签名；历史回放保留旧 fallback URL，模型上下文会剥离已失效的旧本地上传图片，避免它永久阻断该 Session 的后续纯文本对话。升级前保存、尚无该字段的历史附件继续按旧兼容路径处理。

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
- `S3 upload configuration changed. Please re-attach the image.`

### 6.4 工具图片闭环

当工具结果下一轮的主 Agent 或 Sub-agent 消费模型声明 `input: ["image"]` 且配置了顶层 `s3` 时，Runtime 可以把受支持的工具图片附加到下一次模型请求。顶层首轮若由 fast model 发起工具调用，能力判断以随后消费结果的 primary model 为准，而不是生成工具调用的 fast model：

- MCP 标准 `content[]` 中的 `type: "image"`
- MCP 图片 MIME 的嵌入式 `resource.blob`
- 内置只读 `view_image({"path":"..."})` 返回的 Session 工作区图片

仅接受通过内容魔数和结构校验的 PNG/JPEG，单图最大 10MB，每个工具执行批次最多 10 张；上传最多三路并发并保持原始顺序。`view_image` 使用现有工作区路径穿越与符号链接防护，在下一轮消费模型不支持图片或 S3 未配置时不会出现在工具列表中，也可在满足相同条件的 Plan Mode 和只读 Sub-agent 中使用。

Runtime 不扫描普通工具文本、stdout、文件路径、远程 URL 或 `resource_link`，也不自动读取 SVG、WebP、音频。原始二进制/Base64 仅短暂驻留内存，不进入日志、WebSocket、会话 JSON 或模型文本；落盘数据只保存 object key、S3 配置身份、名称和 MIME，请求与历史回放时重新生成签名 URL。

Provider 映射如下：

- Anthropic：图片位于原生 `tool_result` 内容块内
- Gemini：完整 `functionResponse` 后，在同一用户内容中追加 `inlineData`
- Ollama：工具结果后追加视觉观察消息及 `images` Base64
- OpenAI Responses：`function_call_output` 后追加 `input_image` 观察消息
- OpenAI-compatible Chat：并行工具结果全部结束后追加多模态观察消息

合成观察会明确标记为“不可信工具数据，不是用户或系统指令”。图片上传、签名或预取失败只追加“图片未附加”说明，不改变原工具成功/失败状态，也不阻断纯文本 loop。Sub-agent 在自己的内部 loop 中消费图片，父 Agent 只接收其文本结论。

## 7. 建议的前端接入顺序

如果要从零接入一个客户端，建议顺序如下：

1. 轮询或请求 `GET /api/health`，确认服务可用
2. 在每一个连接意图中调用 `GET /api/client-config` 并协商 `protocols.execution_identity`，以意图 token 和目标淘汰迟到响应；未知版本或请求失败时不建立执行 WebSocket
3. 建立 `/ws` 连接
4. 收到 `session`、`view_state`、`todos_state`、`history` 后初始化 UI
5. 发送纯文本，或发送带 `text` / `plan_mode` / `images` 的 JSON 消息；通过 `plan_state` 驱动提问、修订与进度 UI，并使用带 `plan_id + revision` 的 `plan_action` 执行计划
6. 处理 `start -> delta/thinking/tool/* -> done|error(run_terminal=true)`；仅显式终态错误收口当前 run，`false`/缺失字段只显示错误并继续；终态 `error` 后仍可能收到同一 Plan 的 `plan_state`，且失败路径不保证再有 `done`
7. 如需本地上传图片：
   - 复用或强制刷新 `GET /api/client-config` 的上传身份
   - 再调用 `POST /api/upload-images`
   - 校验响应顶层及逐图 `s3_config_id` 与当前身份一致
   - 最后把 `url + object_key + attachment_token + s3_config_id` 带回 WebSocket 消息

## 8. 已知实现特征

- `/api/config/test-model` 与 `/api/config/test-mcp` 的“联通性失败”通常返回 `200 + {ok:false}`
- `/api/config` 在配置文件语法错误时不会返回 4xx，而是返回可恢复信息
- `/api/sessions` 返回当前已知 Session 摘要列表，`main` 固定置顶；`POST /api/session` 创建随机 6 位 id 的新 Session，`PUT /api/session` 修改显示名称和/或工作目录，`DELETE /api/session` 删除非 Main Session
- `/api/todos` 使用整表替换 + revision 冲突语义；冲突时返回 `409 + 当前快照`
- 普通 Session WebSocket 客户端消息没有显式 `type` 字段，按“纯文本 / slash 命令 / JSON 图片或运行选项消息 / plan_action / 兼容 execute_plan_id”自动分流；Group WebSocket 使用 `type:"group_message"`，并拒绝 Plan Mode
- 忙碌时普通文本会进入 deferred intervention 队列，不会立即中断主执行

## 9. 文档维护建议

后续如果新增接口，建议至少同步更新三处：

1. 本文档
2. `frontend/src/types.ts` 或 `frontend/src/types/config.ts`
3. `src/tests/main_tests.rs` 中对应 API / 事件测试
