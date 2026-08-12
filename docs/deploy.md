# LingClaw 部署指南

[简体中文](deploy.md) · [English](deploy.en.md) · [返回 README](../README.md)

LingClaw 由一个 Rust 二进制、`static/` Web 资源以及内置 Skills/Sub-agents 组成。默认监听 `127.0.0.1:18989`，数据位于当前用户的 `~/.lingclaw/`。

> 安装脚本是推荐路径。只运行 `cargo install --path .` 不会自动把 `static/` 和 `docs/reference/` 内容部署到二进制旁边。

## 部署模型

```text
Browser → http://127.0.0.1:18989 → lingclaw process
                                      ├── ~/.lingclaw/.lingclaw.json
                                      ├── lingclaw.db
                                      ├── backups/
                                      └── <session-id>/workspace/
```

- Runtime 始终绑定 loopback，不直接监听 LAN/public interface。
- 本地浏览器直接访问；远程使用 SSH tunnel 或同机反向代理。
- 前端源码在 `frontend/`，Vite 输出到 `static/`，Runtime 只提供构建产物。
- Prompt templates 编译进二进制；系统 Skills/Sub-agents 仍需部署到 `~/.lingclaw/`。

## Windows

### 推荐安装

在 PowerShell 中：

```powershell
git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
```

脚本会：

- 检查 Rust >= 1.90；缺失时通过 `winget` 安装 rustup，rustup 管理的旧工具链会在构建前自动升级 stable。
- 使用最小 Rust 程序验证原生链接器；MSVC C++ Build Tools 缺失时，在正式构建前提供 `winget` 安装选项。
- 检查 Node.js >= 20.19.0 与 npm；需要时安装 Node.js LTS。
- 执行 `frontend\npm ci` 与 `npm run build`；无法准备 Node 时回退到仓库已有 `static/index.html`。
- 使用锁文件和最多 2 个并行任务构建一次 release 二进制，并复用同一构建产物安装到 Cargo bin；失败时保留包含首个错误的完整 Cargo 日志路径。
- 部署 `static/`、系统 Skills 和系统 Sub-agents。
- 运行二进制、静态资源和版本自检。
- 让用户选择 Install、Install-daemon 或暂不全局安装。

直接进入 Setup Wizard 并启动后台服务：

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -Mode InstallDaemon
```

安装后如当前 shell 尚未刷新 PATH，若已接受 PATH 注册，可以重新打开 PowerShell 并运行 `lingclaw`；也可以在当前 shell 中从实际 Cargo 安装目录启动：

```powershell
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
& (Join-Path $cargoHome 'bin\lingclaw.exe')
```

### 手动构建

手动构建同时需要 Rust >= 1.90、Microsoft C++ Build Tools 的“使用 C++ 的桌面开发”工作负载、Node.js >= 20.19.0 与 npm。使用 winget 安装 Build Tools、Rust 和 Node.js：

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Rustlang.Rustup
winget install OpenJS.NodeJS.LTS
```

首次安装后重新打开 PowerShell，再确认工具版本：

```powershell
rustc --version
cargo --version
node --version
npm --version
```

```powershell
git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw

cd frontend
npm ci
npm run build
cd ..

cargo build --release
```

开发产物位于 `target\release\lingclaw.exe`。从仓库根目录直接运行时可以发现仓库中的 `static/`：

```powershell
.\target\release\lingclaw.exe
```

若把二进制复制到其他目录，必须同时复制 `static/`。要保留系统 Skills/Sub-agents，还需部署 `docs/reference/skills/` 与 `docs/reference/agents/`，或者执行：

```powershell
.\target\release\lingclaw.exe install -d (Get-Location).Path
```

该命令直接使用刚构建的开发二进制，因此不要求 `lingclaw` 已经位于 PATH。它会在 npm 可用时构建最新前端；否则要求源码中已有可用的 `static/index.html`。

## Linux

### 推荐安装

```bash
git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw
bash scripts/install-linux.sh
```

脚本支持 Ubuntu/Debian/Kali 与 CentOS/RHEL/Fedora/AlmaLinux/Rocky 的常见依赖路径，并会：

- 安装或复用 Rust >= 1.90；rustup 管理的旧工具链会自动升级 stable。
- 准备 OpenSSL 和 pkg-config 构建依赖。
- 使用 Node.js >= 20.19.0 构建前端；必要时下载临时 Node runtime 或回退系统包。
- 构建一次 release 二进制并复用该产物安装，同时部署 `static/`、系统 Skills 与系统 Sub-agents。
- 运行安装后自检，并可选择 PATH 与 systemd。

### 手动构建

除下面的系统构建依赖外，还需要 Rust >= 1.90、Node.js >= 20.19.0 与 npm。发行版自带的 Rust/Node.js 可能低于要求，请在继续前确认版本：

```bash
rustc --version
node --version
npm --version
```

Ubuntu/Debian 依赖：

```bash
sudo apt-get update
sudo apt-get install -y build-essential libssl-dev pkg-config curl git
```

Fedora/RHEL 系依赖：

```bash
sudo dnf install -y gcc openssl-devel pkgconfig curl git
```

构建和部署：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw

cd frontend
npm ci
npm run build
cd ..

cargo build --release
cargo install --path . --force

CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$CARGO_BIN/static"
cp -R static/. "$CARGO_BIN/static/"
mkdir -p "$HOME/.lingclaw/system-skills" "$HOME/.lingclaw/system-agents"
cp -R docs/reference/skills/. "$HOME/.lingclaw/system-skills/"
cp -R docs/reference/agents/. "$HOME/.lingclaw/system-agents/"
```

如果不重新构建前端，可以使用仓库已有的 `static/`；但它可能不是 `frontend/` 源码对应的最新版本。

## 首次启动与服务管理

如果刚通过推荐 Linux 脚本完成安装，当前 shell 不会继承安装进程临时添加的 PATH。接受 PATH 注册后可以重新打开终端；若要立即启动，直接运行：

```bash
"${CARGO_HOME:-$HOME/.cargo}/bin/lingclaw"
```

已经能从 PATH 找到 LingClaw 时运行：

```bash
lingclaw
```

首次运行进入 Setup Wizard，配置保存在：

- Windows：`%USERPROFILE%\.lingclaw\.lingclaw.json`
- Linux：`~/.lingclaw/.lingclaw.json`

必须显式添加 Provider/Model，并为主 Agent 指定 `primary` 或使用 Session `/model`，普通对话才会启用。详见[配置指南](configuration.md)。

如果用户目录仍包含旧版 `sessions/` 或 `groups/`，首次启动会在开始提供 HTTP 请求前迁移到 `lingclaw.db`。任何损坏、非法 ID 或引用错误都会中止启动并指出文件路径；成功后原目录保存在 `backups/sqlite-migration-<timestamp>/`，不会自动删除。

管理命令：

```bash
lingclaw start          # 后台启动
lingclaw stop           # 优先使用本地认证端点优雅停止
lingclaw restart
lingclaw health         # 快速健康检查
lingclaw status         # 地址、版本、Providers、Models
lingclaw mcp-check      # MCP 深度诊断
lingclaw doctor         # 安装环境检查
lingclaw update         # 从当前源码目录检查、重建、安装、重启
lingclaw install        # 从当前源码目录安装
lingclaw install -d DIR # 从指定源码目录安装
lingclaw db status      # 只读检查 SQLite，不存在时不会创建
lingclaw db backup      # 在线创建并校验一致的 SQLite 快照
lingclaw tui [PATH]     # 从当前或指定项目目录打开终端工作台
lingclaw --version
```

`start` / `restart` 进行受限 MCP preflight；单个 MCP server 失败只产生警告，不阻止服务启动。`mcp-check` 使用运行时超时做更完整的握手和 catalog 诊断。

服务启动后访问 [http://127.0.0.1:18989](http://127.0.0.1:18989)。

### TUI 与工作目录

`lingclaw tui [PATH]` 会先在终端内完成必要的首次模型配置，再检查同一端口的 `/api/health`；服务不存在时复用 daemon launcher 并轮询就绪，退出 TUI 不停止后台服务。PATH 默认为当前目录，必须是现存的绝对可规范化目录。目录可绑定多个 Session；可用 `--session ID` 指定其中一个，用 `--port`、`--lang` 和 `--theme` 覆盖终端选项。首次引导中的 API Key 使用隐藏输入；Raw JSON 在没有 `$VISUAL`/`$EDITOR` 时使用内置编辑器。

私有提示、记忆、Skills、Agents、MCP policy 和缓存始终位于 `~/.lingclaw/<session-id>/workspace/`。外部工作目录只用于文件、Shell、Git、图片、Plan evidence 和 MCP roots。部署备份不能只依赖 `~/.lingclaw/`：如果 Session 绑定了外部项目，需要按项目自身策略另行备份。默认构建支持 Kitty/Sixel/iTerm2 检测；不支持图形协议时仍提供链接与系统查看器降级，使用 `--no-default-features` 可构建纯文本版本。

## systemd

Linux Setup Wizard 可以安装 systemd，也可以手动运行：

```bash
lingclaw systemd-install
sudo systemctl status lingclaw.service
journalctl -u lingclaw.service -f
```

检测到 unit 后，`lingclaw start`、`stop` 和 `restart` 自动转发给 systemd。`install`/`update` 恢复服务时也会重启 unit，不另起 nohup 进程。

unit 固定运行用户、工作目录、`HOME` 与可执行文件路径。Provider/MCP 使用的环境变量需要通过 unit Environment/EnvironmentFile 提供，不能依赖交互 shell 的临时 export。

## 远程访问

### SSH tunnel（推荐）

服务保持 loopback 监听，在客户端执行：

```bash
ssh -L 18989:127.0.0.1:18989 user@server
```

然后本机打开 `http://127.0.0.1:18989`。该方式不改变 LingClaw 网络边界，也不会额外公开端口。

### 反向代理

反向代理必须运行在同一台主机，才能访问 LingClaw 的 loopback 端口。LingClaw 还会校验 `Host` 以及请求中存在的 `Origin`、`Referer` 必须指向 localhost 或 loopback 地址，因此代理必须将这三个请求头改写为本地地址；直接转发公网域名会得到 `403 Forbidden`。

LingClaw 不提供公网用户认证。下面的请求头改写会把外部访问的认证边界交给反向代理；如果代理到外部网络，必须保留 HTTPS 和可靠认证配置。

Nginx 示例：

```nginx
server {
    listen 443 ssl;
    server_name lingclaw.example.com;

    ssl_certificate     /etc/ssl/certs/lingclaw.pem;
    ssl_certificate_key /etc/ssl/private/lingclaw.key;

    auth_basic "LingClaw";
    auth_basic_user_file /etc/nginx/.htpasswd;

    location / {
        proxy_pass http://127.0.0.1:18989;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host 127.0.0.1:18989;
        proxy_set_header Origin http://127.0.0.1:18989;
        proxy_set_header Referer http://127.0.0.1:18989/;
        proxy_read_timeout 3600s;
    }
}
```

只创建防火墙入站规则没有作用，因为 LingClaw 不绑定外部网卡。无法保证请求头改写与可靠认证时，请使用 SSH 隧道；不要使用不带认证的公网反向代理。

MCP Streamable HTTP 的 OAuth 回调地址当前固定为 `http://127.0.0.1:<port>/api/mcp/auth/callback`。远程浏览器仅通过公网反向代理访问时，授权服务会把回调发送到浏览器所在机器的 loopback，而不是 LingClaw 主机，因此无法完成授权。需要使用 MCP OAuth 时，请使用上面的 SSH tunnel，并从转发后的本地 `127.0.0.1:<port>` 打开 LingClaw；反向代理仍可用于不依赖该 OAuth 回调的功能。

## Docker（实验性）

LingClaw 当前没有可配置 listen host，容器内同样绑定 `127.0.0.1`。普通 `docker run -p 18989:18989` 无法访问容器 loopback 上的服务。

在 Linux 上可以使用 host networking：

```dockerfile
FROM node:24-bookworm-slim AS frontend-builder
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY docs/reference/templates/ docs/reference/templates/
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/lingclaw /usr/local/bin/lingclaw
COPY --from=frontend-builder /build/static/ /app/static/
COPY docs/reference/skills/ /app/docs/reference/skills/
COPY docs/reference/agents/ /app/docs/reference/agents/
WORKDIR /app
ENTRYPOINT ["lingclaw", "--serve"]
```

构建并运行：

```bash
docker build -t lingclaw:local .
docker run -d \
  --name lingclaw \
  --network host \
  -v lingclaw-data:/root/.lingclaw \
  -v "$PWD/lingclaw.json:/root/.lingclaw/.lingclaw.json:ro" \
  --restart unless-stopped \
  lingclaw:local
```

上例有意将单个 `lingclaw.json` 以只读方式挂载。此模式下 Settings 可以读取配置，但不能保存配置；请在宿主机编辑 `lingclaw.json` 后运行 `docker restart lingclaw`。

如果需要通过 Settings 保存，请先把配置放在宿主目录中的 `.lingclaw.json`，再用一个可写的目录挂载替换上例的 named volume 和单文件挂载：

```bash
mkdir -p "$PWD/lingclaw-data"
cp lingclaw.json "$PWD/lingclaw-data/.lingclaw.json"

docker run -d \
  --name lingclaw \
  --network host \
  -v "$PWD/lingclaw-data:/root/.lingclaw" \
  --restart unless-stopped \
  lingclaw:local
```

不要同时保留 `-v lingclaw-data:/root/.lingclaw`。LingClaw 的原子保存需要能够在该目录中创建临时文件并替换配置文件；请限制宿主目录权限，因为容器届时可以修改其中的凭据。

容器内不能交互完成 Setup Wizard，应先从 [`.lingclaw.json.example`](../.lingclaw.json.example) 创建 `lingclaw.json`。API Key 推荐通过环境变量传入，而不是写入镜像。

Docker Desktop 是否支持等价 host networking 取决于平台和版本；未确认时不要假设 `-p` 可以绕过 loopback 限制。

## 更新

在源码目录中：

```bash
git pull --ff-only
lingclaw update
```

`update` 会比较源码与已安装版本、停止现有服务、构建后端、准备前端、部署系统 Skills/Sub-agents，并在成功后恢复服务。Windows 会使用临时 helper 释放正在运行的 exe。

更新失败时会尽力恢复之前的服务版本。更新前仍建议备份 `~/.lingclaw/`。

## 备份与恢复

服务运行时可以先创建 SQLite 一致快照：

```bash
lingclaw db status
lingclaw db backup
lingclaw db backup /path/to/lingclaw-snapshot.db
```

默认快照写入 `~/.lingclaw/backups/lingclaw-<timestamp>.db`。命令拒绝覆盖已有目标并在完成后执行完整性校验。它只备份 `lingclaw.db`，不包含配置、MCP OAuth、私有 Session Home 或外部项目工作目录。

停止服务后备份整个数据目录：

```bash
lingclaw stop
tar -czf lingclaw-backup.tar.gz "$HOME/.lingclaw"
```

重点文件：

- `.lingclaw.json`：配置和可能的明文凭据
- `mcp-auth.json`：OAuth tokens
- `lingclaw.db`：Session、Group、消息、Todos、Usage 和 Sub-agent 快照
- `backups/`：手工数据库快照、Schema 升级快照和永久旧 JSON 迁移备份
- `<session-id>/workspace/`：提示、Skills、Agents 和记忆

绑定到其他位置的项目工作目录不在上述归档中，且 Session 删除不会删除它们；请使用项目自身的版本控制或备份策略。

本轮没有 `db restore` 命令。恢复时必须先停止 LingClaw，再还原完整用户目录或用已验证快照替换 `lingclaw.db`，保持相同用户目录权限；确认配置中的本地路径、MCP command 和 S3 identity 后再启动服务。

## 验证与排错

```bash
lingclaw --version
lingclaw doctor
lingclaw start
lingclaw health
lingclaw status
lingclaw mcp-check
```

常见问题：

| 症状 | 检查 |
|---|---|
| 首页 404 | `static/index.html` 是否部署到二进制旁边，或当前工作目录是否包含 `static/` |
| 没有内置 Skills/Agents | `~/.lingclaw/system-skills/` 与 `system-agents/` 是否存在 |
| 发送按钮禁用 | Provider 模型和 Agent `primary` 是否显式配置 |
| 配置修改未生效 | 手工编辑后是否重启；Settings 是否保存成功 |
| MCP 加载失败 | `lingclaw mcp-check`、command/url、cwd、环境变量和 OAuth |
| 图片上传不可用 | `enableS3`、S3 字段、生命周期检查和模型 `input` |
| 远程连接失败 | 是否使用 SSH tunnel/同机代理；服务不会监听外部网卡 |

后端日志包含运行与错误信息，但不应公开分享含本机路径、Provider 响应或 MCP 配置的完整日志。
