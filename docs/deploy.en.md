# LingClaw Deployment

[简体中文](deploy.md) · [English](deploy.en.md) · [Back to README](../README.en.md)

LingClaw consists of one Rust binary, `static/` web assets, and bundled skills/sub-agents. It listens on `127.0.0.1:18989` by default and stores data under the current user's `~/.lingclaw/`.

> The install scripts are the recommended path. Running only `cargo install --path .` does not deploy `static/` or `docs/reference/` content beside the binary.

## Deployment model

```text
Browser → http://127.0.0.1:18989 → lingclaw process
                                      ├── ~/.lingclaw/.lingclaw.json
                                      ├── lingclaw.db
                                      ├── backups/
                                      └── <session-id>/workspace/
```

- The runtime always binds loopback, never a LAN/public interface directly.
- A local browser connects directly; remote users need an SSH tunnel or same-host reverse proxy.
- Frontend source is in `frontend/`, Vite writes `static/`, and the runtime serves the build output only.
- Prompt templates are compiled into the binary; bundled skills/sub-agents still need deployment under `~/.lingclaw/`.

## Windows

### Recommended installation

In PowerShell:

```powershell
git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
```

The script:

- Checks Rust >= 1.90, installs rustup through `winget` when missing, and upgrades an older rustup-managed stable toolchain before building.
- Compiles a minimal Rust native-linker probe; when the MSVC C++ Build Tools workload is missing, it offers a `winget` installation before the main build.
- Checks Node.js >= 20.19.0 and npm, installing Node.js LTS when needed.
- Runs `frontend\npm ci` and `npm run build`; if Node cannot be prepared, it falls back to an existing `static/index.html`.
- Builds the release binary once from the lockfile with at most two parallel jobs, reuses that artifact when installing into Cargo bin, and preserves the complete Cargo log path when a build fails.
- Deploys `static/`, bundled skills, and bundled sub-agents.
- Verifies the binary, frontend asset, and version command.
- Offers Install, Install-daemon, or no global installation yet.

Open the Setup Wizard and start the daemon immediately:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -Mode InstallDaemon
```

If the current shell has not refreshed PATH and you accepted PATH registration, reopen PowerShell and run `lingclaw`. Alternatively, start it from the effective Cargo installation directory in the current shell:

```powershell
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
& (Join-Path $cargoHome 'bin\lingclaw.exe')
```

### Manual build

A manual build also requires Rust >= 1.90, the Microsoft C++ Build Tools "Desktop development with C++" workload, Node.js >= 20.19.0, and npm. Install Build Tools, Rust, and Node.js with winget:

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install Rustlang.Rustup
winget install OpenJS.NodeJS.LTS
```

After a first-time installation, reopen PowerShell and verify the tool versions:

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

The development binary is `target\release\lingclaw.exe`. Running from the repository root lets it discover the repository `static/`:

```powershell
.\target\release\lingclaw.exe
```

When moving the binary elsewhere, copy `static/` as well. To retain bundled skills/sub-agents, deploy `docs/reference/skills/` and `docs/reference/agents/`, or run:

```powershell
.\target\release\lingclaw.exe install -d (Get-Location).Path
```

This invokes the development binary that was just built, so it does not require `lingclaw` to be on PATH. It builds the current frontend when npm is available; otherwise the source must contain a valid `static/index.html`.

## Linux

### Recommended installation

```bash
git clone https://github.com/Linkq123/LingClaw.git
cd LingClaw
bash scripts/install-linux.sh
```

The script supports common dependency paths for Ubuntu/Debian/Kali and CentOS/RHEL/Fedora/AlmaLinux/Rocky. It:

- Installs or reuses Rust >= 1.90 and upgrades an older rustup-managed stable toolchain.
- Prepares OpenSSL and pkg-config build dependencies.
- Builds the frontend with Node.js >= 20.19.0, downloading a temporary runtime or using system packages when needed.
- Builds the release binary once, reuses that artifact for installation, and deploys `static/`, bundled skills, and bundled sub-agents.
- Runs post-install checks and can configure PATH and systemd.

### Manual build

In addition to the system build dependencies below, install Rust >= 1.90, Node.js >= 20.19.0, and npm. Distribution packages may be older than required, so verify the versions before continuing:

```bash
rustc --version
node --version
npm --version
```

Ubuntu/Debian dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential libssl-dev pkg-config curl git
```

Fedora/RHEL-family dependencies:

```bash
sudo dnf install -y gcc openssl-devel pkgconfig curl git
```

Build and deploy:

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

You may use the repository's existing `static/` without rebuilding the frontend, but it may not match the latest `frontend/` source.

## First launch and service management

After running the recommended Linux installer, the current shell does not inherit the temporary PATH set inside the installer process. If you accepted PATH registration, reopen the terminal; to start immediately, run:

```bash
"${CARGO_HOME:-$HOME/.cargo}/bin/lingclaw"
```

When LingClaw is already available on PATH, run:

```bash
lingclaw
```

The first run opens the Setup Wizard. Configuration is stored at:

- Windows: `%USERPROFILE%\.lingclaw\.lingclaw.json`
- Linux: `~/.lingclaw/.lingclaw.json`

Normal chat remains disabled until a provider/model and main-agent `primary` (or session `/model`) are explicit. See [Configuration](configuration.en.md).

If the user directory still contains legacy `sessions/` or `groups/`, the first launch migrates them into `lingclaw.db` before serving HTTP requests. Corruption, an invalid ID, or a broken reference stops startup and identifies the file. On success, the original directories stay permanently under `backups/sqlite-migration-<timestamp>/`.

Management commands:

```bash
lingclaw start          # Start in the background
lingclaw stop           # Prefer the authenticated local graceful endpoint
lingclaw restart
lingclaw health         # Fast health check
lingclaw status         # Address, version, providers, models
lingclaw mcp-check      # Deep MCP diagnostics
lingclaw doctor         # Installation environment checks
lingclaw update         # Check, rebuild, install, and restart from source
lingclaw install        # Install from the current source directory
lingclaw install -d DIR # Install from a chosen source directory
lingclaw db status      # Inspect SQLite read-only; never creates a missing DB
lingclaw db backup      # Create and verify a consistent online SQLite snapshot
lingclaw tui [PATH]     # Open the terminal workspace from this or another project
lingclaw --version
```

`start` and `restart` perform a bounded MCP preflight. A failed server warns but does not prevent service startup. `mcp-check` uses runtime timeouts for a more complete handshake and catalog diagnosis.

After startup, open [http://127.0.0.1:18989](http://127.0.0.1:18989).

### TUI and working directories

`lingclaw tui [PATH]` completes any required first-time model setup inside the terminal before checking `/api/health` on the selected port. It reuses the daemon launcher when no service exists and polls until ready; exiting the TUI leaves the daemon running. PATH defaults to the current directory and must be an existing, canonicalizable absolute directory. A directory may bind several sessions; use `--session ID` to choose one, with `--port`, `--lang`, and `--theme` for terminal overrides. The setup masks API-key input, and Raw JSON uses the built-in editor when `$VISUAL`/`$EDITOR` is absent.

Private prompts, memory, skills, agents, MCP policy, and caches always stay under `~/.lingclaw/<session-id>/workspace/`. An external working directory is used only for files, shell, Git, images, Plan evidence, and MCP roots. A deployment backup therefore cannot rely on `~/.lingclaw/` alone when sessions bind external projects. Default builds detect Kitty/Sixel/iTerm2; unsupported terminals retain the link and system-viewer fallback, while `--no-default-features` produces a text-only build.

## systemd

The Linux Setup Wizard can install systemd, or run it manually:

```bash
lingclaw systemd-install
sudo systemctl status lingclaw.service
journalctl -u lingclaw.service -f
```

When the unit exists, `lingclaw start`, `stop`, and `restart` delegate to systemd. Service recovery after `install`/`update` restarts the unit instead of launching an extra nohup process.

The unit fixes the user, working directory, `HOME`, and executable path. Provider/MCP environment variables must be supplied through unit Environment/EnvironmentFile and cannot depend on an interactive shell export.

## Remote access

### SSH tunnel (recommended)

Keep loopback binding and run on the client:

```bash
ssh -L 18989:127.0.0.1:18989 user@server
```

Then open `http://127.0.0.1:18989` locally. This does not change LingClaw's network boundary or publish a new service port.

### Reverse proxy

The proxy must run on the same host to reach LingClaw's loopback port. LingClaw also requires `Host` and any `Origin` or `Referer` headers present in the request to identify localhost or a loopback address. The proxy must therefore rewrite all three headers to the local endpoint; forwarding the public hostname directly results in `403 Forbidden`.

LingClaw does not provide public-user authentication. These header rewrites place the external access boundary at the reverse proxy, so keep the HTTPS and strong-authentication configuration when exposing it to another network.

Nginx example:

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

Opening a firewall rule alone has no effect because LingClaw does not bind an external interface. If you cannot guarantee the header rewrites and strong authentication, use an SSH tunnel; never publish an unauthenticated proxy.

The OAuth callback for Streamable HTTP MCP is currently fixed to `http://127.0.0.1:<port>/api/mcp/auth/callback`. When a remote browser uses only the public reverse proxy, the authorization server redirects to the browser machine's loopback rather than the LingClaw host, so authorization cannot complete. Use the SSH tunnel above for MCP OAuth and open LingClaw through the forwarded local `127.0.0.1:<port>`. The reverse proxy remains suitable for features that do not depend on this OAuth callback.

## Docker (experimental)

LingClaw currently has no configurable listen host and still binds `127.0.0.1` inside a container. Ordinary `docker run -p 18989:18989` cannot reach the service on the container loopback interface.

Linux host networking is a workable experimental path:

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

Build and run:

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

The example intentionally mounts the individual `lingclaw.json` file read-only. Settings can read configuration in this mode but cannot save it; edit `lingclaw.json` on the host and run `docker restart lingclaw`.

To save through Settings, first place the configuration at `.lingclaw.json` inside a host directory, then replace both mounts from the example with one read-write directory mount:

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

Do not keep `-v lingclaw-data:/root/.lingclaw` at the same time. LingClaw's atomic save needs permission to create a temporary file and replace the configuration file in that directory. Restrict the host-directory permissions because the container can then modify stored credentials.

The container cannot complete the interactive Setup Wizard. Create `lingclaw.json` from [`.lingclaw.json.example`](../.lingclaw.json.example) first. Prefer environment variables for API keys instead of embedding them in the image.

Equivalent host networking on Docker Desktop depends on platform and version. Do not assume `-p` bypasses the loopback limitation.

## Updating

From the source directory:

```bash
git pull --ff-only
lingclaw update
```

`update` compares source and installed versions, stops the service, builds the backend, prepares the frontend, deploys bundled skills/sub-agents, and restores the service after success. Windows uses a temporary helper to release the running exe.

On failure, the updater attempts to restore the previous service. Back up `~/.lingclaw/` before significant updates.

## Backup and restore

Create a consistent SQLite snapshot while the service is running:

```bash
lingclaw db status
lingclaw db backup
lingclaw db backup /path/to/lingclaw-snapshot.db
```

The default destination is `~/.lingclaw/backups/lingclaw-<timestamp>.db`. The command refuses to overwrite an existing target and verifies integrity after completion. It backs up `lingclaw.db` only, not configuration, MCP OAuth, private Session Homes, or external project working directories.

Stop the service and archive the full data directory:

```bash
lingclaw stop
tar -czf lingclaw-backup.tar.gz "$HOME/.lingclaw"
```

Important data:

- `.lingclaw.json` — Configuration and possible plaintext credentials
- `mcp-auth.json` — OAuth tokens
- `lingclaw.db` — Sessions, groups, messages, todos, usage, and sub-agent snapshots
- `backups/` — Manual snapshots, schema-upgrade snapshots, and permanent legacy-JSON migration backups
- `<session-id>/workspace/` — Prompts, skills, agents, and memory

Project working directories bound elsewhere are not part of this archive, and session deletion never removes them. Protect them through their own version-control or backup policy.

There is no `db restore` command in this release. Stop LingClaw before restoring the complete user directory or replacing `lingclaw.db` with a verified snapshot. Preserve user ownership and verify local paths, MCP commands, and S3 identity before starting the service.

## Verification and troubleshooting

```bash
lingclaw --version
lingclaw doctor
lingclaw start
lingclaw health
lingclaw status
lingclaw mcp-check
```

| Symptom | Check |
|---|---|
| Home page returns 404 | `static/index.html` beside the binary or `static/` under the working directory |
| Bundled skills/agents missing | `~/.lingclaw/system-skills/` and `system-agents/` exist |
| Send button disabled | Provider model and agent `primary` are explicit |
| Configuration edit ignored | Restart after manual edits; verify Settings saved successfully |
| MCP load failure | `lingclaw mcp-check`, command/url, cwd, environment, OAuth |
| Image upload unavailable | `enableS3`, S3 fields, lifecycle check, and model `input` |
| Remote connection fails | Use SSH tunnel/same-host proxy; the service never listens on external interfaces |

Backend logs may contain local paths, provider responses, or MCP configuration. Do not post complete logs publicly without review.
