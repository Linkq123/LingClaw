# LingClaw 部署指南

LingClaw 是单二进制 + 单静态文件的架构，部署极其简单。首次启动时会进入交互式 Setup Wizard，引导你配置 API Provider、Key 和默认模型，配置保存在 `~/.lingclaw/.lingclaw.json`。

默认 Web 端口为 `18989`。

---

## 1. Windows

### 1.1 从源码构建

```powershell
# 安装 Rust（如尚未安装）
winget install Rustlang.Rustup

# 克隆并构建
git clone <repo-url> LingClaw
cd LingClaw
cargo build --release
```

产物位于 `target\release\lingclaw.exe`。

### 1.2 运行

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

### 1.3 防火墙

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
- 执行 `cargo build --release`
- 将 `static/` 前端资源部署到 cargo bin 目录旁边，避免首页 404
- 执行安装后自检，确认 `lingclaw` 二进制和 `static/index.html` 都已就位
- 最后让你选择 `Install`、`Install-daemon` 或 `Skip for now`
- `Install` 路径会继续询问是否持久化 PATH、是否添加 systemd 服务

手动构建流程如下：

```bash
# 安装 Rust（如尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 克隆并构建
git clone <repo-url> LingClaw
cd LingClaw
cargo build --release
cargo install --path .
mkdir -p "${CARGO_HOME:-$HOME/.cargo}/bin/static"
cp -R static/. "${CARGO_HOME:-$HOME/.cargo}/bin/static/"
mkdir -p "$HOME/.lingclaw/system-skills" "$HOME/.lingclaw/system-agents"
cp -R docs/reference/skills/. "$HOME/.lingclaw/system-skills/"
cp -R docs/reference/agents/. "$HOME/.lingclaw/system-agents/"
```
如果只执行 `cargo install --path .` 而没有同步部署 `static/`、`docs/reference/skills/`、`docs/reference/agents/`，`/api/health` 仍可能正常，但访问首页 `http://127.0.0.1:18989/` 会返回 404，且内置 Skills / Sub-Agents 不可用。

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
# ── 构建阶段 ──
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
COPY static/ /app/static/
COPY docs/reference/skills/ /app/docs/reference/skills/
COPY docs/reference/agents/ /app/docs/reference/agents/

WORKDIR /app
EXPOSE 18989

ENV LINGCLAW_PORT=18989
ENTRYPOINT ["lingclaw", "--serve"]
```

> Docker 场景下可通过挂载 `~/.lingclaw/.lingclaw.json` 配置文件，也可通过 `-e` 传入环境变量作为覆盖。Prompt 模板已在编译期内嵌，运行时不要求容器内存在 `docs/reference/templates/`；但内置 Skills / Sub-Agents 依赖运行时磁盘目录发现，因此镜像中需要保留 `docs/reference/skills/` 和 `docs/reference/agents/`，否则它们不会被加载。

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
| `lingclaw-data` | 持久化会话数据和 workspace（`~/.lingclaw/sessions/`、`~/.lingclaw/{sessionId}/workspace/`） |
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

---

## 配置参考

所有配置通过 `~/.lingclaw/.lingclaw.json` 管理（首次运行 Setup Wizard 自动创建）。参见 `.lingclaw.json.example` 获取完整示例。

### settings 字段

| JSON 字段 | 默认值 | 说明 | 环境变量覆盖 |
|-----------|--------|------|--------------|
| `port` | `18989` | HTTP 监听端口 | `LINGCLAW_PORT` |
| `provider` | `"auto"` | 强制指定：`openai` / `anthropic` / `ollama` / `auto` | `LINGCLAW_PROVIDER` |
| `apiKey` | — | 通用 API Key（若未使用 providers 多配置） | `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `OLLAMA_API_KEY` |
| `apiBase` | 按 provider 默认 | API 端点地址 | `OPENAI_API_BASE` / `OLLAMA_API_BASE` |
| `execTimeout` | `30` | Shell 命令超时（秒） | `LINGCLAW_EXEC_TIMEOUT` |
| `maxContextTokens` | `32000` | 上下文窗口 Token 预算 | `LINGCLAW_MAX_CONTEXT_TOKENS` |
| `maxOutputBytes` | `51200` | 工具输出截断阈值 | — |
| `maxFileBytes` | `204800` | 文件读取大小上限 | — |

> 优先级：JSON 配置文件 > 环境变量 > 内置默认值

## 文件结构

```
lingclaw                        # 二进制
static/
  index.html                    # WebChat 前端
docs/reference/templates/       # 7 个 Prompt 模板（BOOTSTRAP/AGENTS/IDENTITY/SOUL/USER/TOOLS/MEMORY.md）
docs/reference/skills/          # 内置 system skills（运行时磁盘发现）
docs/reference/agents/          # 内置 system sub-agents（运行时磁盘发现）
~/.lingclaw/
  .lingclaw.json                # 配置文件（Setup Wizard 自动创建）
  sessions/                     # 磁盘持久化的会话 JSON
  {sessionId}/workspace/        # 每会话隔离工作区（含 7 个 prompt 文件 + memory/ 日志）
```

其中 `docs/reference/templates/` 是可选的磁盘覆盖目录：

- 编译时：必须存在，二进制会把这些模板内嵌进去。
- 运行时：不是必需目录；若存在，则优先使用磁盘上的模板内容。

## 验证部署

```bash
# 健康检查
curl http://127.0.0.1:18989/api/health

# 预期返回
# {"status":"ok","model":"gpt-4o-mini","sessions":0}
```

浏览器打开 `http://<host>:18989` 即可使用。

聊天页交互说明：

- Agent 运行时，输入框里发送普通文本会作为“延迟干预”排队，不会打断当前 ReAct 周期；这些输入会在下一轮 Analyze 前送入模型
- Agent 运行时，发送按钮会切换为停止按钮，点击后等价于发送 `/stop`
- Agent 运行时，只允许 `/stop`、`/tool`、`/reasoning` 这类运行期控制命令立即执行；其余斜杠命令需要等当前轮次结束后再发送
