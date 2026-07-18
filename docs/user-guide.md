# LingClaw 用户指南

[简体中文](user-guide.md) · [English](user-guide.en.md) · [返回 README](../README.md)

本指南介绍 LingClaw 的日常使用方式。安装和服务管理见[部署指南](deploy.md)，模型、MCP 与 S3 设置见[配置指南](configuration.md)。

## 核心概念

### Main 与 Session

`main` 是永久存在的主 Session，也是 Group 的隐式 Owner。每个 Session 都有独立的：

- 消息历史和视图状态
- Todos 快照
- 模型覆盖
- MCP 与系统 Skills 权限
- `~/.lingclaw/<session-id>/workspace/` 工作区
- 提示文件、Skills、Agents 和记忆

创建 Session 时由后端生成 6 位英数字 ID。显示名称可以修改，但 ID、工作区路径和协议引用不会改变。删除 Session 是不可恢复操作：它会删除 Session 存档和整个 `~/.lingclaw/<session-id>/` 目录，包括工作区文件、提示、Skills、Agents 和记忆，并从所有 Group 中移除该成员。`main`、当前已连接或正在运行的 Session 不能删除。

会话导航按名称或 ID 搜索。最近区域最多显示 12 项，并始终包含 Main 和当前会话；其余内容收进“更早会话”。

### Group

Group 把多个 Session 组织为群聊。Main 负责治理，但不会作为普通 dispatch member 重复执行成员任务。群聊支持三种目标模式：

| 模式 | 行为 |
|---|---|
| 全部 | 向所有有效成员派发；每个被派发成员都必须回复 |
| 已选 | 只向成员选择器中勾选的 Session 派发 |
| @提及 | 只向消息中的有效 `@session-id` 派发；使用 `@all` 时，未被直接点名的扩展成员可以返回 `NO_REPLY`；缺少有效提及时禁止发送 |

界面使用成员名称展示提及，发往后端的协议仍使用 `@session-id`。成员必须拥有有效的全局 Agent 模型或 Session `/model` 覆盖，否则不会启动运行。

### Model、Provider 与 Agent role

Provider 描述 API 端点和协议，Model 描述可用的模型 ID、上下文、输出、推理和输入能力。Agent role 决定某项工作默认使用哪个已配置模型：

- `primary`：主对话
- `fast`：轻量辅助调用
- `sub-agent`：默认 Sub-agent
- `memory`：Structured Memory
- `reflection`：Daily Reflection
- `context`：上下文压缩
- `sub-agent-<name>`：特定 Sub-agent 覆盖

Session 的 `/model` 覆盖只影响该 Session，并优先于全局 `primary`。无效覆盖不会静默回退。

## 工作台

### 会话导航

桌面端左侧栏用于创建、搜索和切换 Session/Group，底部进入 Settings、Usage、主题和语言。桌面展开偏好保存在 `lingclaw.sessionDrawerExpanded`；手机导航只保存在内存，选择会话、点击遮罩或按 Escape 后关闭。

Session/Group 的创建、编辑、重命名、删除和移除成员使用应用内对话框。对话框支持键盘焦点循环、Escape、遮罩关闭、内联校验、异步错误和提交忙碌态。

### 对话与执行栈

用户消息右对齐，Assistant Markdown 使用自然排版。每轮动态过程聚合为一个 Execution Stack：

- Reasoning
- Tool call/result
- Task Plan
- Sub-agent
- Orchestration

运行时默认展开；完成后折叠成摘要。用户手动更改折叠状态后，界面保留选择。Tools 与 Reasoning 视图开关按步骤类型过滤；关联内容隐藏时会关闭对应 Inspector 或弹层。

工具步骤点击后打开 Tool Inspector。桌面宽屏停靠在右侧，中等宽度使用浮动抽屉，手机使用底部面板。图片结果在 Inspector 内以懒加载画廊展示。

### 输入区

- `Enter` 发送，`Shift+Enter` 换行。
- 输入首字符 `/` 打开命令菜单；方向键切换，`Tab` 补全。
- Group 中输入 `@` 打开成员补全；写入协议的是 Session ID。
- `+` 菜单包含图片附件和 Plan Mode；能力不可用时对应入口隐藏或禁用。
- Agent 运行中发送普通文本会排队，在下一个 Analyze 阶段前注入；停止按钮和 `/stop` 会立即请求取消。
- 模型不可用时，输入框显示原因并禁用普通发送；状态、帮助和设置类命令仍可使用。

### Todos

`todos` 是当前 Session 唯一的结构化任务清单。前后端使用整表替换和 `revision` 乐观并发：

- 用户可以在 Todo 面板编辑状态和内容。
- Agent 通过 `todos` 工具提交完整清单。
- revision 冲突返回最新快照，防止旧页面覆盖新数据。
- `/clear` 会清空消息和 Todos，并推进 revision。

### Usage

Usage 页面展示今日与累计 Token、模型角色拆分、每日趋势和 Provider 趋势。角色包括 Primary、Fast、Sub-Agent、Memory、Reflection 和 Context。全零数据使用一个空状态；只有部分模块无数据时，仅对应区域显示空状态。

## Slash Commands

| 命令 | 说明 |
|---|---|
| `/new` | 把对话摘要写入每日记忆并清空当前上下文 |
| `/model [name]` | 查看模型或切换当前 Session 模型 |
| `/switch <id>` | 切换 Session |
| `/sessions` | 列出 Session |
| `/delete <id>` | 删除非 Main、非当前且未运行的 Session |
| `/think [level]` | 设置 `auto`、`off`、`minimal`、`low`、`medium`、`high`、`xhigh` 或 `max` |
| `/react [on\|off]` | 切换 ReAct 阶段可见性 |
| `/tool [on\|off]` | 切换 Tool 步骤显示并持久化视图状态 |
| `/reasoning [on\|off]` | 切换 Reasoning 显示并持久化视图状态 |
| `/stop` | 中断当前 Agent run |
| `/skills` | 列出工具和全部已发现 Skills |
| `/skills-system [install\|uninstall <pattern>]` | 管理当前 Session 的系统 Skills |
| `/skills-global` | 列出全局 Skills |
| `/skills-session` | 列出当前 Session Skills |
| `/agents` | 列出 Sub-agents、来源和有效工具 |
| `/status` | 显示模型、Provider、运行阶段、上下文与思考状态 |
| `/system-prompt` | 显示当前系统提示及估算 Token |
| `/mcp [refresh]` | 查看 MCP 状态；`refresh` 强制刷新 catalog |
| `/usage` | 显示当前和今日 Token 摘要 |
| `/clear` | 清空消息和 Todos，保留系统提示 |
| `/memory [stats\|debug]` | 查看 Structured Memory 状态与审计 |
| `/reflection [today\|yesterday\|list]` | 查看 Daily Reflection |
| `/help` | 显示命令帮助 |

Group socket 不执行普通 Session slash command。请先切回 Session 再运行命令。

## 内置工具

| 工具 | 用途 |
|---|---|
| `think` | Agent 内部推理便签 |
| `todos` | 原子替换 Session Todo 清单 |
| `exec` | 运行带超时和危险命令检测的 shell 命令 |
| `read_file` | 读取文件或指定行范围 |
| `write_file` | 创建或覆写文件 |
| `patch_file` | 按精确片段修改文件 |
| `delete_file` | 删除工作区文件 |
| `list_dir` | 列出目录 |
| `search_files` | 正则搜索工作区 |
| `http_fetch` | 带 SSRF 与重定向防护的 HTTP GET |
| `view_image` | 条件化读取工作区 PNG/JPEG |
| `task` | 向一个 Sub-agent 委托任务 |
| `orchestrate` | 按 DAG 编排多个 Sub-agent |
| `session_control` | 仅 Main 正常模式可用的 Session/Group 调度与治理 |

`view_image` 只在当前消费模型声明 `input: ["image"]` 且 S3 可用时暴露。Plan Mode 只提供只读工具、满足策略的只读 MCP 工具，以及条件化 `view_image`。

## Skills

Skill 是包含 `SKILL.md` 的知识模块。LingClaw 按以下顺序发现，同名时后者覆盖前者：

| 层级 | 目录 |
|---|---|
| System | `~/.lingclaw/system-skills/` |
| Global | `~/.lingclaw/skills/` |
| Session | `~/.lingclaw/<session-id>/workspace/skills/` |

最小格式：

```markdown
---
name: my-skill
description: 说明能力和触发条件
---

# Instructions

执行步骤、约束和参考资源。
```

LingClaw 先把名称、来源和描述注入系统提示，Agent 匹配任务后再通过文件工具读取完整内容和引用资源。系统 Skills 默认不注入，需要在 Settings → Skills 或 `/skills-system install` 为当前 Session 启用。完整内置清单见[系统 Skills](system-skills.zh-CN.md)。

## Sub-agents 与 Orchestration

Sub-agent 从三层目录发现：

| 层级 | 目录 |
|---|---|
| System | `~/.lingclaw/system-agents/` |
| Global | `~/.lingclaw/agents/` |
| Session | `~/.lingclaw/<session-id>/workspace/agents/` |

内置代理包括 `explore`、`researcher`、`frontend-coder`、`backend-coder`、`general-coder` 和 `reviewer`。`AGENT.md` frontmatter 可以设置 `max_turns` 及 `tools.allow` / `tools.deny`；过滤同时作用于内置工具和 `mcp__...` 工具。

模型解析顺序：

1. `agents.defaults.model.sub-agent-<name>`
2. `agents.defaults.model.sub-agent`
3. 父 Session 的有效模型

Sub-agent 有独立消息历史、工具集和 ReAct loop。为防止递归与竞争，`task`、`orchestrate` 和 Session `todos` 不会暴露给 Sub-agent。`subAgentTimeout` 限制总时间，取消父 run 也会取消子任务。

`orchestrate` 按依赖层并发执行任务；依赖失败时后续任务会失败或跳过。主界面显示完成数、失败数和任务图，详情中保留每个任务的阶段、工具链与结果。

## Plan Mode、Task Plan 与思考

- **Plan Mode**：本轮只做理解、只读探索和计划输出。计划写入历史后，用户点击“开始执行”进入普通模式。
- **Task Plan**：Settings 中的可选运行期软指导，在每轮 Analyze 前根据目标、证据和工具生成，不代表已经执行。
- **Think level**：控制支持推理模型的 effort。`auto` 根据任务信号选择级别，Auto Debug 只在本地展示最近一条决策轨迹。
- **Reasoning visibility**：只控制界面是否展示，不改变 Provider 返回或 Agent 行为。

## 记忆与上下文

工作区中的 `MEMORY.md` 由用户维护。`memory/YYYY-MM-DD.md` 保存每日日志与 `/new` 摘要。

- Structured Memory 开启后，后台抽取稳定偏好、项目上下文和事实到 `structured_memory.json`，并写入审计日志。
- Daily Reflection 开启后，多步任务完成时可能在后台追加简短反思到每日日志。
- 上下文接近上限时，LingClaw 会裁剪或压缩旧消息，并发送可见通知；原始 Session 持久化仍由运行时管理。
- Memory、Reflection 和 Context 的辅助调用计入各自 Usage 角色，但不会重复消费工具图片。

## 图片

### 用户附件

配置 S3 且当前 Session 的有效模型声明 `input: ["image"]` 后，输入区 `+` 菜单可以上传 PNG/JPEG。每条消息最多 10 张，单张最多 10MB。Session 历史只保存 object key、S3 配置身份、名称和 MIME，请求时重新签名 URL。

### 工具图片闭环

Runtime 从 MCP `image`、图片型 `resource.blob` 和 `view_image` 提取经过魔数验证的 PNG/JPEG，上传后附加到对应 `tool` 消息供下一 cycle 使用。普通文本中的路径、URL、SVG、WebP 和音频不会被自动读取。

图片缺失、上传失败或文本模型不支持图片时，原工具结果仍然完成，并向 Agent 与 Inspector 加入文字说明。Sub-agent 可以在自己的 loop 中消费图片，但不会把图片重复传给父 Agent。

## 下一步

- [配置模型、MCP 与 S3](configuration.md)
- [部署与服务管理](deploy.md)
- [理解 Runtime 架构](architecture.md)
- [查阅 HTTP 与 WebSocket 协议](backend-api.md)
