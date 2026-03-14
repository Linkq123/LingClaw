# LingClaw 部署指南

LingClaw 是单二进制 + 单静态文件的架构，部署极其简单。首次启动时会进入交互式 Setup Wizard，引导你配置 API Provider、Key 和默认模型，配置保存在 `~/.lingclaw/.lingclaw.json`。

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

配置文件位于 `%USERPROFILE%\.lingclaw\.lingclaw.json`，支持手动编辑。参见项目根目录 `lingclaw.json.example` 获取完整配置示例。

LingClaw 默认以后台守护进程运行，通过 CLI 命令管理：

```powershell
lingclaw start      # 启动服务
lingclaw stop       # 停止服务
lingclaw restart    # 重启服务
lingclaw health     # 健康检查
lingclaw status     # 详细状态（地址、版本、providers、models）
lingclaw update     # 检查版本，有更新时 rebuild 并重启
lingclaw help       # 查看帮助信息
lingclaw --version  # 显示版本号
```

浏览器打开 `http://127.0.0.1:3000`。

### 1.3 防火墙

如需局域网访问，放通端口：

```powershell
New-NetFirewallRule -DisplayName "LingClaw" -Direction Inbound -LocalPort 3000 -Protocol TCP -Action Allow
```

---

## 2. Linux

### 2.1 从源码构建

```bash
# 安装 Rust（如尚未安装）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 克隆并构建
git clone <repo-url> LingClaw
cd LingClaw
cargo build --release
```

产物位于 `target/release/lingclaw`。

### 2.2 运行

```bash
# 首次运行 — Setup Wizard 完成后自动后台启动
./target/release/lingclaw

# 重新配置
./target/release/lingclaw --install-daemon
```

配置文件位于 `~/.lingclaw/.lingclaw.json`，支持手动编辑。

CLI 管理命令（开启 PATH 后可直接使用）：

```bash
lingclaw start      # 启动服务
lingclaw stop       # 停止服务
lingclaw restart    # 重启服务
lingclaw health     # 健康检查
lingclaw status     # 详细状态（含版本号）
lingclaw update     # 检查版本，有更新时 rebuild 并重启
lingclaw help       # 查看帮助信息
lingclaw --version  # 显示版本号
```

### 2.3 systemd 服务（可选）

> LingClaw 已内置守护进程管理（`lingclaw start/stop/restart`），通常无需 systemd。如需开机自启可配置 systemd 服务。

创建 `/etc/systemd/system/lingclaw.service`：

```ini
[Unit]
Description=LingClaw AI Assistant
After=network.target

[Service]
Type=simple
User=lingclaw
WorkingDirectory=/opt/lingclaw
ExecStart=/opt/lingclaw/lingclaw --serve
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
# 部署
sudo cp target/release/lingclaw /opt/lingclaw/
sudo cp -r static /opt/lingclaw/
sudo useradd -r -s /bin/false lingclaw
sudo chown -R lingclaw:lingclaw /opt/lingclaw

sudo systemctl daemon-reload
sudo systemctl enable --now lingclaw
sudo systemctl status lingclaw
```

### 2.4 反向代理（可选）

Nginx 示例，提供 HTTPS + WebSocket 代理：

```nginx
server {
    listen 443 ssl;
    server_name lingclaw.example.com;

    ssl_certificate     /etc/ssl/certs/lingclaw.pem;
    ssl_certificate_key /etc/ssl/private/lingclaw.key;

    location / {
        proxy_pass http://127.0.0.1:3000;
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
RUN cargo build --release --locked 2>/dev/null || cargo build --release

# ── 运行阶段 ──
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/lingclaw /usr/local/bin/
COPY static/ /app/static/

WORKDIR /app
EXPOSE 3000

ENV LINGCLAW_PORT=3000
ENTRYPOINT ["lingclaw", "--serve"]
```

> Docker 场景下可通过挂载 `~/.lingclaw/.lingclaw.json` 配置文件，也可通过 `-e` 传入环境变量作为覆盖。

### 3.2 构建镜像

```bash
docker build -t lingclaw:latest .
```

### 3.3 运行容器

> Docker 容器使用 `--serve` 前台模式运行。需提前挂载配置文件（容器内无法交互式运行 Setup Wizard）。

```bash
docker run -d \
  --name lingclaw \
  -p 3000:3000 \
  -v /path/to/.lingclaw.json:/root/.lingclaw/.lingclaw.json:ro \
  -v lingclaw-sessions:/app/.lingclaw \
  lingclaw:latest
```

| 挂载卷 | 用途 |
|--------|------|
| `.lingclaw.json` | 配置文件（必须，容器不支持 Setup Wizard） |
| `lingclaw-sessions` | 持久化 `.lingclaw/sessions/` 会话数据 |

### 3.4 Docker Compose

```yaml
services:
  lingclaw:
    build: .
    ports:
      - "3000:3000"
    volumes:
      - ./lingclaw.json:/root/.lingclaw/.lingclaw.json:ro
      - lingclaw-sessions:/app/.lingclaw
    restart: unless-stopped

volumes:
  lingclaw-sessions:
```

> 将 `lingclaw.json.example` 复制为 `lingclaw.json` 并编辑后挂载即可。

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
      "model": { "primary": "anthropic/claude-sonnet-4-20250514" }
    }
  }
}
```

---

## 配置参考

所有配置通过 `~/.lingclaw/.lingclaw.json` 管理（首次运行 Setup Wizard 自动创建）。参见 `lingclaw.json.example` 获取完整示例。

### settings 字段

| JSON 字段 | 默认值 | 说明 | 环境变量覆盖 |
|-----------|--------|------|--------------|
| `port` | `3000` | HTTP 监听端口 | `LINGCLAW_PORT` |
| `provider` | `"auto"` | 强制指定：`openai` / `anthropic` / `auto` | `LINGCLAW_PROVIDER` |
| `apiKey` | — | 通用 API Key（若未使用 providers 多配置） | `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` |
| `apiBase` | 按 provider 默认 | API 端点地址 | `OPENAI_API_BASE` |
| `execTimeout` | `30` | Shell 命令超时（秒） | `LINGCLAW_EXEC_TIMEOUT` |
| `maxToolRounds` | `20` | 每条消息最大工具调用轮数 | `LINGCLAW_MAX_TOOL_ROUNDS` |
| `maxContextTokens` | `32000` | 上下文窗口 Token 预算 | `LINGCLAW_MAX_CONTEXT_TOKENS` |
| `maxOutputBytes` | `51200` | 工具输出截断阈值 | — |
| `maxFileBytes` | `204800` | 文件读取大小上限 | — |

> 优先级：JSON 配置文件 > 环境变量 > 内置默认值

## 文件结构

```
lingclaw           # 二进制
static/
  index.html       # WebChat 前端
~/.lingclaw/
  .lingclaw.json   # 配置文件（Setup Wizard 自动创建）
.lingclaw/
  sessions/        # 磁盘持久化的会话 JSON
```

## 验证部署

```bash
# 健康检查
curl http://127.0.0.1:3000/api/health

# 预期返回
# {"status":"ok","model":"gpt-4o-mini","sessions":0}
```

浏览器打开 `http://<host>:3000` 即可使用。
