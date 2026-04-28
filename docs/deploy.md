# LingClaw 部署指南

LingClaw 是单二进制 + 一组静态前端资源的架构，部署仍然很简单。前端源码位于 `frontend/`，通过 Vite 构建输出到 `static/`；运行时实际读取的是 `static/` 目录。普通源码包可直接使用仓库内已有的 `static/`，如果你改动了 `frontend/`，部署前需要先重新生成 `static/`。安装脚本会优先自动补齐 Node.js / `npm` 并重建前端；只有在你走手动构建流程时，才需要自己先准备 Node.js（建议当前 LTS 版本）。首次启动时会进入交互式 Setup Wizard，引导你配置 API Provider、Key 和默认模型，配置保存在 `~/.lingclaw/.lingclaw.json`。

默认 Web 端口为 `18989`。

---

## 1. Windows

### 1.1 直接安装（推荐）

推荐直接使用安装脚本：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
```

脚本会自动：

- 检查 Rust 环境；若未安装则通过 `winget` 安装 `rustup`
- 若缺少 Node.js / `npm`，或现有版本过旧，则通过 `winget` 自动安装符合前端构建要求的 Node.js LTS，并执行 `frontend\npm ci` 和 `npm run build`
- 如果 Node.js 自动安装失败但仓库里已有 `static/index.html`，则回退到现有静态产物继续安装
- 在 Windows 下预处理 `target\release\lingclaw.exe` 的占用问题，避免 `cargo build --release` 被旧文件卡住
- 执行 `cargo build --release` 和 `cargo install --path . --force`
- 将 `static/` 部署到 cargo bin 目录旁边，避免首页 404
- 将 `docs/reference/skills/` 和 `docs/reference/agents/` 安装到 `%USERPROFILE%\.lingclaw\system-skills\`、`%USERPROFILE%\.lingclaw\system-agents\`
- 执行安装后自检，确认 `lingclaw.exe` 和 `static/index.html` 都已就位
- 最后让你选择 `Install`、`Install-daemon` 或 `Skip for now`
- `Install` 路径会继续询问是否写入用户 PATH

如果你想安装后立刻进入 Setup Wizard 并自动拉起后台服务，可以直接：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -Mode InstallDaemon
```

### 1.2 手动从源码构建

```powershell
# 安装 Rust（如尚未安装）
winget install Rustlang.Rustup

# 克隆并构建
git clone <repo-url> LingClaw
cd LingClaw
cargo build --release
```

如果你修改过 `frontend/`，或需要确保 Web UI 使用的是最新前端产物，再额外执行：

```powershell
cd frontend
npm ci
npm run build
cd ..
```

产物位于 `target\release\lingclaw.exe`。

直接从源码目录运行 `target\release\lingclaw.exe` 时，程序会自动从项目根目录发现 `static/`。如果你只把 `lingclaw.exe` 单独复制到别处运行，需要同时复制 `static/` 目录；若还要保留内置 Skills / Sub-Agents，则还需一并部署 `docs/reference/skills/` 和 `docs/reference/agents/`，或直接使用 `lingclaw install`。

`lingclaw install -d <source-dir>` 会先构建 Rust 二进制；若检测到 `frontend/package.json` 且 `npm` 可用，还会自动执行前端构建并安装最新 `static/`。如果 `npm` 不可用但源码目录里已有可用的 `static/index.html`，则会回退到安装现有静态产物。`lingclaw update` 在源码目录内升级时，前端处理逻辑与这里保持一致。

### 1.3 运行

```powershell
# 首次运行 — 进入 Setup Wizard，完成后自动后台启动
.\target\release\lingclaw.exe

# 重新配置 — 强制进入 Setup Wizard（已有配置自动备份，不覆盖历史备份）
.\target\release\lingclaw.exe --install-daemon
```

配置文件位于 `%USERPROFILE%\.lingclaw\.lingclaw.json`，支持手动编辑。参见项目根目录 `.lingclaw.json.example` 获取完整配置示例。

LingClaw 默认以后台守护进程运行，通过 CLI 命令管理：

```powershell
lingclaw start      # 启动服务
lingclaw stop       # 停止服务
lingclaw restart    # 重启服务
lingclaw health     # 健康检查
lingclaw status     # 详细状态（地址、版本、providers、models）
lingclaw mcp-check  # 深度检查 MCP server 连接与工具发现
lingclaw update     # 检查版本，有更新时 rebuild 并重启
lingclaw install    # 从本地源码安装（当前目录）
lingclaw install -d E:\path\to\src  # 从指定目录安装
lingclaw help       # 查看帮助信息
lingclaw --version  # 显示版本号
```

浏览器打开 `http://127.0.0.1:18989`。

### 1.4 防火墙

如需局域网访问，放通端口：

```powershell
New-NetFirewallRule -DisplayName "LingClaw" -Direction Inbound -LocalPort 18989 -Protocol TCP -Action Allow
```

---

## 2. Linux

### 2.1 从源码构建

推荐直接使用安装脚本：

```bash
bash scripts/install-linux.sh
```

脚本会自动：

- 检查 Rust 环境；若未安装则自动安装，已安装时跳过 Rust 安装本身
- 按 Linux 发行版安装 `openssl` / `pkg-config` 构建依赖
- 若缺少 Node.js / `npm`，或现有版本过旧，则优先自动下载符合前端构建要求的 Node.js LTS；必要时再回退到支持的发行版安装方式，并重建最新前端产物
- 如果 Node.js 自动安装失败但仓库里已有 `static/index.html`，则回退到现有静态产物继续安装
- 执行 `cargo build --release`
- 将 `static/` 前端资源部署到 cargo bin 目录旁边，避免首页 404
- 执行安装后自检，确认 `lingclaw` 二进制和 `static/index.html` 都已就位
- 最后让你选择 `Install`、`Install-daemon` 或 `Skip for now`
- `Install` 路径会继续询问是否持久化 PATH、是否添加 systemd 服务

说明：安装脚本会优先尝试自动构建最新的 `frontend/` 产物，并把生成后的 `static/` 一并部署。如果当前系统不支持自动安装 Node.js / `npm`，或者自动安装失败，但仓库里已经有可用的 `static/index.html`，脚本会回退到部署现有 `static/`。

手动构建流程如下：

```bash
# 安装 Rust（如尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 克隆并构建
git clone <repo-url> LingClaw
cd LingClaw

# 若修改了 frontend/ 或需要重建 Web UI，再额外执行
cd frontend
npm ci
npm run build
cd ..

cargo build --release
cargo install --path .
mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin/static"
cp -R static/. "${CARGO_HOME:-$HOME/.cargo}/bin/static/"
mkdir -p "$HOME/.lingclaw/system-skills" "$HOME/.lingclaw/system-agents"
cp -R docs/reference/skills/. "$HOME/.lingclaw/system-skills/"
cp -R docs/reference/agents/. "$HOME/.lingclaw/system-agents/"
```
如果只执行 `cargo install --path .` 而没有同步部署 `static/`、`docs/reference/skills/`、`docs/reference/agents/`，`/api/health` 仍可能正常，但访问首页 `http://127.0.0.1:18989/` 会返回 404，且内置 Skills / Sub-Agents 不可用。如果你改动过 `frontend/` 却没有重新执行 Vite 构建，服务虽然能启动，但页面仍会停留在旧版静态资源。

如果安装报错 `error: failed to run custom build command for openssl-sys`：
- Ubuntu / Debian / Kali Linux
```bash
sudo apt-get update
sudo apt-get install -y libssl-dev pkg-config
```
- CentOS / RHEL / Fedora / AlmaLinux
```bash
# CentOS/RHEL/AlmaLinux
sudo yum install -y openssl-devel pkgconfig
# Fedora
sudo dnf install -y openssl-devel pkgconfig
```

产物位于 `target/release/lingclaw`。

### 2.2 运行

```bash
# 首次运行 — Setup Wizard 完成后自动后台启动
./target/release/lingclaw

# 重新配置
./target/release/lingclaw --install-daemon
```

配置文件位于 `~/.lingclaw/.lingclaw.json`，支持手动编辑。Linux 下 Setup Wizard 会额外询问是否添加 `systemd` 服务（yes/no）。

CLI 管理命令（开启 PATH 后可直接使用）：

```bash
lingclaw start      # 启动服务
lingclaw stop       # 停止服务
lingclaw restart    # 重启服务
lingclaw health     # 健康检查
lingclaw status     # 详细状态（含版本号）
lingclaw mcp-check  # 深度检查 MCP server 连接与工具发现
lingclaw update     # 检查版本，有更新时 rebuild 并重启
lingclaw install    # 从本地源码安装（当前目录）
lingclaw install -d /path/to/src  # 从指定目录安装
lingclaw systemd-install    # 安装并启用 lingclaw.service
lingclaw help       # 查看帮助信息
lingclaw --version  # 显示版本号
```

`lingclaw install -d /path/to/src` 的前端行为与 Windows 相同：优先自动构建 `frontend/`，无法构建时再回退到现有 `static/`。

说明：

- `start` / `restart` 会先执行受限的一次性 MCP preflight；失败只会给出警告，不会阻止服务启动
- `mcp-check` 会按运行时超时配置做更深的 MCP 诊断，适合排查 `command`、`cwd`、协议握手或工具发现问题
- 浏览器聊天页可用 `/mcp` 查看当前已加载的 MCP server 状态，`/mcp refresh` 会清空缓存、空闲会话和最近失败冷却状态并重新探测 tools；服务端发出的 `notifications/tools/list_changed` 也会让下一次工具发现自动刷新
- MCP server 连续启动失败后会进入短暂冷却，避免每次请求都反复拉起失败进程；手动执行 `/mcp refresh` 可立即清除该冷却并重试
- 运行时的 MCP 空闲会话会自动回收，因此 `mcp-check` 的一次性诊断进程和聊天页的长生命周期工具会话不会相互复用
- `stop` 会优先走本地认证的优雅关停端点 `/api/shutdown`，超时后才回退到强制结束进程

### 2.3 systemd 服务（可选）

推荐直接在 Setup Wizard 里选择 `YES`，或在安装完成后运行：

```bash
lingclaw systemd-install

# 查看状态与日志
sudo systemctl status lingclaw.service
journalctl -u lingclaw.service -f
```

配置了 `systemd` 后，`lingclaw start`、`stop`、`restart` 会自动转为管理 `lingclaw.service`。`install` / `update` 触发服务恢复时，也会重启这个服务，而不是再额外起一个 `nohup` 进程。
`systemd-install` 生成的 unit 会对可执行文件、工作目录和 `HOME` 环境值做引用处理，因此安装路径或家目录包含空格时也能正确启动。

### 2.4 反向代理（可选）

Nginx 示例，提供 HTTPS + WebSocket 代理：

```nginx
server {
    listen 443 ssl;
    server_name lingclaw.example.com;

    ssl_certificate     /etc/ssl/certs/lingclaw.pem;
    ssl_certificate_key /etc/ssl/private/lingclaw.key;

    location / {
        proxy_pass http://127.0.0.1:18989;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_read_timeout 3600s;
    }
}
```

---

## 3. Docker

### 3.1 Dockerfile

在项目根目录创建 `Dockerfile`：

```dockerfile
# ── 前端构建阶段 ──
FROM node:20-bookworm-slim AS frontend-builder
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# ── Rust 构建阶段 ──
FROM rust:1.85-slim AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/
COPY docs/reference/templates/ docs/reference/templates/
RUN cargo build --release --locked 2>/dev/null || cargo build --release

# ── 运行阶段 ──
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/lingclaw /usr/local/bin/
COPY --from=frontend-builder /build/static/ /app/static/
COPY docs/reference/skills/ /app/docs/reference/skills/
COPY docs/reference/agents/ /app/docs/reference/agents/

WORKDIR /app
EXPOSE 18989

ENV LINGCLAW_PORT=18989
ENTRYPOINT ["lingclaw", "--serve"]
```

> 这个 Dockerfile 会直接从 `frontend/` 重新构建最新的 `static/`，不依赖仓库里已有的前端产物。Docker 场景下可通过挂载 `~/.lingclaw/.lingclaw.json` 配置文件，也可通过 `-e` 传入环境变量作为覆盖。Prompt 模板已在编译期内嵌，运行时不要求容器内存在 `docs/reference/templates/`；但内置 Skills / Sub-Agents 依赖运行时磁盘目录发现，因此镜像中需要保留 `docs/reference/skills/` 和 `docs/reference/agents/`，否则它们不会被加载。

### 3.2 构建镜像

```bash
docker build -t lingclaw:latest .
```

### 3.3 运行容器

> Docker 容器使用 `--serve` 前台模式运行。需提前挂载配置文件（容器内无法交互式运行 Setup Wizard）。

```bash
docker run -d \
  --name lingclaw \
  -p 18989:18989 \
  -v lingclaw-data:/root/.lingclaw \
  -v /path/to/.lingclaw.json:/root/.lingclaw/.lingclaw.json:ro \
  lingclaw:latest
```

| 挂载卷 | 用途 |
|--------|------|
| `lingclaw-data` | 持久化主会话数据和工作区（`~/.lingclaw/sessions/main.json`、`~/.lingclaw/main/workspace/`） |
| `.lingclaw.json` | 配置文件（必须，容器不支持 Setup Wizard；bind mount 覆盖卷内同路径） |

### 3.4 Docker Compose

```yaml
services:
  lingclaw:
    build: .
    ports:
      - "18989:18989"
    volumes:
      - lingclaw-data:/root/.lingclaw
      - ./lingclaw.json:/root/.lingclaw/.lingclaw.json:ro
    restart: unless-stopped

volumes:
  lingclaw-data:
```

> 将 `.lingclaw.json.example` 复制为 `lingclaw.json` 并编辑后挂载即可。

### 3.5 使用 Anthropic

在 `lingclaw.json` 中配置 Anthropic provider：

```json
{
  "models": {
    "providers": {
      "anthropic": {
        "baseUrl": "https://api.anthropic.com",
        "apiKey": "sk-ant-xxx",
        "api": "anthropic",
        "models": [{ "id": "claude-sonnet-4-20250514" }]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "anthropic/claude-sonnet-4-20250514",
        "fast": "anthropic/claude-haiku-3-20250306"
      }
    }
  }
}
```

### 3.6 使用 Gemini

在 `lingclaw.json` 中配置 Gemini provider：

```json
{
  "models": {
    "providers": {
      "gemini": {
        "baseUrl": "https://generativelanguage.googleapis.com/v1beta",
        "apiKey": "AIza-xxx",
        "api": "gemini",
        "models": [{ "id": "gemini-2.5-flash", "input": ["text", "image"] }]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "gemini/gemini-2.5-flash"
      }
    }
  }
}
```

也可以用环境变量快速启动：`GEMINI_API_KEY=AIza-xxx LINGCLAW_PROVIDER=gemini LINGCLAW_MODEL=gemini-2.5-flash lingclaw`。Gemini 图片输入使用本地预取后的 `inlineData`，因此与 Ollama 一样可以配合私网或 localhost 的 S3-compatible 图片网关。

---

## 配置参考

所有配置通过 `~/.lingclaw/.lingclaw.json` 管理（首次运行 Setup Wizard 自动创建）。参见 `.lingclaw.json.example` 获取完整示例。

### settings 字段

| JSON 字段 | 默认值 | 说明 | 环境变量覆盖 |
|-----------|--------|------|--------------|
| `port` | `18989` | HTTP 监听端口 | `LINGCLAW_PORT` |
| `provider` | `"auto"` | 遗留兼容字段；仅在未使用 `models.providers` 时用于强制指定 `openai` / `anthropic` / `ollama` / `gemini` / `auto` | `LINGCLAW_PROVIDER` |
| `apiKey` | — | 遗留兼容字段；新配置应优先写入 `models.providers.*.apiKey` | `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `OLLAMA_API_KEY` / `GEMINI_API_KEY` / `GOOGLE_API_KEY` |
| `apiBase` | 按 provider 默认 | 遗留兼容字段；新配置应优先写入 `models.providers.*.baseUrl` | `OPENAI_API_BASE` / `OLLAMA_API_BASE` / `GEMINI_API_BASE` |
| `execTimeout` | `30` | Shell 命令超时（秒） | `LINGCLAW_EXEC_TIMEOUT` |
| `toolTimeout` | `30` | 非 shell 的 Act 阶段工具预算；MCP 默认也继承这个超时 | `LINGCLAW_TOOL_TIMEOUT` |
| `subAgentTimeout` | `300` | 子代理总执行超时（秒，`0` 表示不限时） | `LINGCLAW_SUB_AGENT_TIMEOUT` |
| `maxLlmRetries` | `2` | LLM HTTP 层瞬态错误重试次数（429/5xx/连接/超时） | `LINGCLAW_MAX_LLM_RETRIES` |
| `maxContextTokens` | `32000` | 上下文窗口 Token 预算 | `LINGCLAW_MAX_CONTEXT_TOKENS` |
| `maxOutputBytes` | `51200` | 工具输出截断阈值 | — |
| `maxFileBytes` | `204800` | 文件读取大小上限 | — |
| `structuredMemory` | `false` | 启用 Finish 后台结构化记忆抽取与后续 prompt 注入 | `LINGCLAW_STRUCTURED_MEMORY` |
| `dailyReflection` | `false` | 启用多步任务完成后的后台 reflection 写入 | `LINGCLAW_DAILY_REFLECTION` |
| `enableS3` | `true` | 开启本地图片上传能力；仍需顶层 `s3` 配置完整可用 | `LINGCLAW_ENABLE_S3` |

> 优先级：JSON 配置文件 > 环境变量 > 内置默认值

> 新配置建议：用 `models.providers` 定义 provider 实例，用 `agents.defaults.model.primary` 选择默认模型。Setup Wizard 已不再写入 `settings.provider`、`settings.apiKey`、`settings.apiBase`。

### agents.defaults.model 字段

| JSON 字段 | 说明 | 环境变量覆盖 |
|-----------|------|--------------|
| `primary` | 主 Agent 默认模型 | `LINGCLAW_MODEL` |
| `fast` | 简单首轮查询优先使用的轻量模型；若当前上下文含图片，则仅在该模型支持图片输入时启用 | `LINGCLAW_FAST_MODEL` |
| `sub-agent` | 子代理执行模型 | `LINGCLAW_SUB_AGENT_MODEL` |
| `sub-agent-<name>` | 指定子代理的专属模型；未配置时回退到 `sub-agent` 再回退到 `primary` | - |
| `memory` | structured memory 后台抽取模型 | `LINGCLAW_MEMORY_MODEL` |
| `reflection` | daily reflection 后台模型 | `LINGCLAW_REFLECTION_MODEL` |
| `context` | 自动上下文压缩优先模型 | `LINGCLAW_CONTEXT_MODEL` |

### mcpServers 与 s3

- `mcpServers` 是顶层对象，每个 server 可配置 `command`、`args`、`env`、`cwd`、`timeoutSecs`
- `mcpServers.*.cwd` 必须位于当前主会话工作区 `~/.lingclaw/main/workspace/` 内，否则会被拒绝启动
- `mcpServers.*.timeoutSecs` 未设置时继承 `toolTimeout`
- 顶层 `s3` 用于本地 JPEG/PNG 上传；OpenAI/Anthropic 需要可被远端 provider 访问的现签 URL，私网 endpoint 推荐配合 Gemini/Ollama 使用

## 文件结构

```
lingclaw                        # 二进制
frontend/                       # 前端源码（TypeScript + React + Vite）
  src/                          # 应用源码
  package.json                  # 前端依赖与脚本
  vite.config.ts                # Vite 构建配置（输出到 ../static/）
static/                         # 运行时前端产物（由 frontend/ 构建生成）
  index.html                    # WebChat 入口
  assets/                       # 打包后的 JS/CSS/字体等资源
  branding/                     # 品牌资源
docs/reference/templates/       # 7 个 Prompt 模板（BOOTSTRAP/AGENTS/IDENTITY/SOUL/USER/TOOLS/MEMORY.md）
docs/reference/skills/          # 内置 system skills（运行时磁盘发现）
docs/reference/agents/          # 内置 system sub-agents（运行时磁盘发现）
~/.lingclaw/
  .lingclaw.json                # 配置文件（Setup Wizard 自动创建）
  sessions/                     # 磁盘持久化的会话 JSON
    main.json                   # 单主会话存档
  main/workspace/               # 主会话工作区（含 7 个 prompt 文件 + memory/ 日志）
```

其中 `docs/reference/templates/` 是可选的磁盘覆盖目录：

- 编译时：必须存在，二进制会把这些模板内嵌进去。
- 运行时：不是必需目录；若存在，则优先使用磁盘上的模板内容。

## 验证部署

```bash
# 健康检查
curl http://127.0.0.1:18989/api/health

# 预期返回
# {"status":"ok","version":"0.6.1","model":"gpt-4o-mini","sessions":1}
```

浏览器打开 `http://<host>:18989` 即可使用。

聊天页交互说明：

- Agent 运行时，输入框里发送普通文本会作为“延迟干预”排队，不会打断当前 ReAct 周期；这些输入会在下一轮 Analyze 前送入模型
- Agent 运行时，发送按钮会切换为停止按钮，点击后等价于发送 `/stop`
- Agent 运行时，只允许 `/stop`、`/tool`、`/reasoning` 这类运行期控制命令立即执行；其余斜杠命令需要等当前轮次结束后再发送
- Usage 页会同时展示 Provider 维度和 Model Role 维度的 Token Breakdown；`/api/usage` 也会返回 `daily_roles`、`total_roles` 以及 `usage_history[].roles`
- 聊天页中由斜杠命令返回的 `success`、`system`、`error` 卡片支持手动关闭；运行中通知（如 `progress`、`context_pruned`、`context_compressed`）不会提供关闭按钮
- 当已发现多个子代理时，模型可调用 `orchestrate` 工具进行 DAG 编排；前端会收到 `orchestrate_started`、`orchestrate_layer`、`orchestrate_task_*`、`orchestrate_completed` 事件用于展示执行进度
