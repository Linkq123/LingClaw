#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

MIN_NODE_VERSION="20.19.0"
PREFERRED_NODE_MAJOR="24"
TEMP_NODE_DIR=""

cleanup_temp_node() {
  if [[ -n "$TEMP_NODE_DIR" && -d "$TEMP_NODE_DIR" ]]; then
    rm -rf "$TEMP_NODE_DIR"
  fi
}

trap cleanup_temp_node EXIT

info() {
  printf '[LingClaw] %s\n' "$1"
}

warn() {
  printf '[LingClaw] WARNING: %s\n' "$1" >&2
}

prompt_yes_no() {
  local prompt="$1"
  local answer
  read -r -p "$prompt [y/N] " answer
  [[ "$answer" =~ ^([yY][eE][sS]|[yY])$ ]]
}

ensure_rust() {
  if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    info "Rust environment already installed: $(rustc --version)"
    info 'No additional Rust environment installation is required.'
    return 0
  fi

  info 'Rust environment not found. Installing via rustup.'
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    warn 'Rust installation did not finish correctly. Please check rustup output and retry.'
    exit 1
  fi
}

install_build_deps() {
  if [[ ! -f /etc/os-release ]]; then
    warn 'Cannot detect Linux distribution. Skipping automatic OpenSSL/pkg-config dependency installation.'
    return
  fi

  # shellcheck disable=SC1091
  source /etc/os-release
  local family="${ID:-} ${ID_LIKE:-}"

  if [[ "$family" =~ (ubuntu|debian|kali) ]]; then
    info 'Installing build dependencies for Ubuntu / Debian / Kali Linux.'
    sudo apt-get update
    sudo apt-get install -y libssl-dev pkg-config
    return
  fi

  if [[ "$family" =~ (centos|rhel|fedora|almalinux|rocky) ]]; then
    if [[ "${ID:-}" == "fedora" || "${ID_LIKE:-}" =~ fedora ]]; then
      info 'Installing build dependencies for Fedora.'
      sudo dnf install -y openssl-devel pkgconfig
    else
      info 'Installing build dependencies for CentOS / RHEL / AlmaLinux.'
      if command -v yum >/dev/null 2>&1; then
        sudo yum install -y openssl-devel pkgconfig
      else
        sudo dnf install -y openssl-devel pkgconfig
      fi
    fi
    return
  fi

  warn "Unsupported Linux distribution (${ID:-unknown}). Please install OpenSSL and pkg-config development packages manually if cargo build fails."
}

version_gte() {
  local lhs="${1#v}"
  local rhs="${2#v}"
  local lhs_major lhs_minor lhs_patch rhs_major rhs_minor rhs_patch
  IFS=. read -r lhs_major lhs_minor lhs_patch <<<"$lhs"
  IFS=. read -r rhs_major rhs_minor rhs_patch <<<"$rhs"
  lhs_major="${lhs_major:-0}"
  lhs_minor="${lhs_minor:-0}"
  lhs_patch="${lhs_patch:-0}"
  rhs_major="${rhs_major:-0}"
  rhs_minor="${rhs_minor:-0}"
  rhs_patch="${rhs_patch:-0}"

  if ((10#$lhs_major != 10#$rhs_major)); then
    ((10#$lhs_major > 10#$rhs_major))
    return
  fi
  if ((10#$lhs_minor != 10#$rhs_minor)); then
    ((10#$lhs_minor > 10#$rhs_minor))
    return
  fi
  ((10#$lhs_patch >= 10#$rhs_patch))
}

current_node_version() {
  if ! command -v node >/dev/null 2>&1; then
    return 1
  fi
  node --version 2>/dev/null | tr -d '\r' | sed 's/^v//'
}

download_node_runtime() {
  if ! command -v curl >/dev/null 2>&1; then
    warn 'curl is unavailable. Cannot download a compatible Node.js runtime automatically.'
    return 1
  fi
  if ! command -v tar >/dev/null 2>&1; then
    warn 'tar is unavailable. Cannot unpack a compatible Node.js runtime automatically.'
    return 1
  fi

  local arch raw_arch base_url manifest archive tmp_dir archive_path node_dir
  raw_arch="$(uname -m)"
  case "$raw_arch" in
    x86_64|amd64) arch="x64" ;;
    aarch64|arm64) arch="arm64" ;;
    *)
      warn "Unsupported Linux architecture for automatic Node.js download: $raw_arch"
      return 1
      ;;
  esac

  base_url="https://nodejs.org/dist/latest-v${PREFERRED_NODE_MAJOR}.x"
  info "Downloading Node.js LTS runtime for frontend build (${raw_arch})."
  manifest="$(curl -fsSL "$base_url/SHASUMS256.txt")" || {
    warn 'Failed to fetch the Node.js release manifest.'
    return 1
  }

  archive="$(printf '%s\n' "$manifest" | awk "/linux-${arch}\\.tar\\.xz$/ { print \$2; exit }")"
  if [[ -z "$archive" ]]; then
    warn "Could not find a Node.js archive for linux-${arch} in the release manifest."
    return 1
  fi

  tmp_dir="$(mktemp -d)"
  archive_path="$tmp_dir/$archive"
  if ! curl -fsSL "$base_url/$archive" -o "$archive_path"; then
    warn 'Failed to download the Node.js runtime archive.'
    rm -rf "$tmp_dir"
    return 1
  fi
  if ! tar -xf "$archive_path" -C "$tmp_dir"; then
    warn 'Failed to unpack the Node.js runtime archive.'
    rm -rf "$tmp_dir"
    return 1
  fi

  node_dir="$(find "$tmp_dir" -maxdepth 1 -mindepth 1 -type d -name 'node-v*' | head -n 1)"
  if [[ -z "$node_dir" || ! -x "$node_dir/bin/node" ]]; then
    warn 'Downloaded Node.js runtime is incomplete.'
    rm -rf "$tmp_dir"
    return 1
  fi

  TEMP_NODE_DIR="$tmp_dir"
  export PATH="$node_dir/bin:$PATH"
  hash -r
  return 0
}

install_node_runtime_from_package_manager() {
  if [[ ! -f /etc/os-release ]]; then
    warn 'Cannot detect Linux distribution. Skipping automatic Node.js / npm installation.'
    return 1
  fi

  # shellcheck disable=SC1091
  source /etc/os-release
  local family="${ID:-} ${ID_LIKE:-}"

  if [[ "$family" =~ (ubuntu|debian|kali) ]]; then
    info 'Installing Node.js / npm for Ubuntu / Debian / Kali Linux.'
    if ! sudo apt-get update; then
      warn 'apt-get update failed while preparing Node.js / npm.'
      return 1
    fi
    if ! sudo apt-get install -y nodejs npm; then
      warn 'apt-get install failed while preparing Node.js / npm.'
      return 1
    fi
    return 0
  fi

  if [[ "$family" =~ (centos|rhel|fedora|almalinux|rocky) ]]; then
    if [[ "${ID:-}" == "fedora" || "${ID_LIKE:-}" =~ fedora ]]; then
      info 'Installing Node.js / npm for Fedora.'
      if ! sudo dnf install -y nodejs npm; then
        warn 'dnf install failed while preparing Node.js / npm.'
        return 1
      fi
    else
      info 'Installing Node.js / npm for CentOS / RHEL / AlmaLinux.'
      if command -v yum >/dev/null 2>&1; then
        if ! sudo yum install -y nodejs npm; then
          warn 'yum install failed while preparing Node.js / npm.'
          return 1
        fi
      else
        if ! sudo dnf install -y nodejs npm; then
          warn 'dnf install failed while preparing Node.js / npm.'
          return 1
        fi
      fi
    fi
    return 0
  fi

  warn "Unsupported Linux distribution (${ID:-unknown}). Skipping automatic Node.js / npm installation."
  return 1
}

install_node_runtime() {
  if download_node_runtime; then
    return 0
  fi
  install_node_runtime_from_package_manager
}

ensure_node() {
  local node_version=""
  if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    node_version="$(current_node_version || true)"
    if [[ -n "$node_version" ]] && version_gte "$node_version" "$MIN_NODE_VERSION"; then
      return 0
    fi
    if [[ -n "$node_version" ]]; then
      warn "Node.js $node_version is below the required minimum $MIN_NODE_VERSION. Attempting automatic upgrade."
    fi
  fi

  warn 'Node.js / npm are unavailable or too old. Attempting automatic installation.'
  if ! install_node_runtime; then
    return 1
  fi

  if command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1; then
    node_version="$(current_node_version || true)"
    if [[ -z "$node_version" ]]; then
      warn 'Node.js installation finished but the runtime version could not be determined.'
      return 1
    fi
    if ! version_gte "$node_version" "$MIN_NODE_VERSION"; then
      warn "Node.js installation finished but the available runtime is still below $MIN_NODE_VERSION."
      return 1
    fi
    info "Node.js environment installed: $(node --version)"
    return 0
  fi

  warn 'Node.js installation finished but node/npm are still unavailable.'
  return 1
}

install_release() {
  local cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  cargo install --path . --force
  if [[ -d "$ROOT_DIR/static" ]]; then
    mkdir -p "$cargo_bin/static"
    cp -R "$ROOT_DIR/static/." "$cargo_bin/static/"
    info "Installed frontend assets to $cargo_bin/static"
  else
    warn 'Static frontend assets directory not found; web UI may return 404.'
  fi
  # Install system skills to ~/.lingclaw/system-skills/
  local system_skills_src="$ROOT_DIR/docs/reference/skills"
  local system_skills_dst="$HOME/.lingclaw/system-skills"
  if [[ -d "$system_skills_src" ]]; then
    rm -rf "$system_skills_dst"
    mkdir -p "$system_skills_dst"
    cp -R "$system_skills_src/." "$system_skills_dst/"
    info "Installed system skills to $system_skills_dst"
  fi
  # Install system agents to ~/.lingclaw/system-agents/
  local system_agents_src="$ROOT_DIR/docs/reference/agents"
  local system_agents_dst="$HOME/.lingclaw/system-agents"
  if [[ -d "$system_agents_src" ]]; then
    rm -rf "$system_agents_dst"
    mkdir -p "$system_agents_dst"
    cp -R "$system_agents_src/." "$system_agents_dst/"
    info "Installed system agents to $system_agents_dst"
  fi
  export PATH="$cargo_bin:$PATH"
}

post_install_self_check() {
  local cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  local lingclaw_bin="$cargo_bin/lingclaw"
  local static_index="$cargo_bin/static/index.html"
  local failed=0

  info 'Running post-install self-check.'

  if [[ -x "$lingclaw_bin" ]]; then
    info "Binary check passed: $lingclaw_bin"
  else
    warn "Binary check failed: $lingclaw_bin is missing or not executable."
    failed=1
  fi

  if [[ -f "$static_index" ]]; then
    info "Frontend asset check passed: $static_index"
  else
    warn "Frontend asset check failed: $static_index is missing. Web UI may return 404."
    failed=1
  fi

  if [[ $failed -eq 0 ]]; then
    if "$lingclaw_bin" --version >/dev/null 2>&1; then
      info 'CLI self-check passed: lingclaw --version'
      info 'Install self-check passed.'
      return 0
    fi
    warn 'CLI self-check failed: lingclaw --version returned non-zero status.'
    failed=1
  fi

  warn 'Install self-check failed. Re-run the installer or manually verify ~/.cargo/bin and ~/.cargo/bin/static.'
  return 1
}

run_install_choice() {
  local choice="$1"
  local lingclaw_bin="${CARGO_HOME:-$HOME/.cargo}/bin/lingclaw"

  case "$choice" in
    Install)
      info 'Installing LingClaw into the global cargo bin directory.'
      install_release
      post_install_self_check
      if prompt_yes_no 'Add LingClaw to PATH for future shells?'; then
        "$lingclaw_bin" path-install
      fi
      if prompt_yes_no 'Add systemd service now?'; then
        "$lingclaw_bin" systemd-install
      fi
      ;;
    Install-daemon)
      info 'Installing LingClaw and launching the setup wizard.'
      install_release
      post_install_self_check
      "$lingclaw_bin" --install-daemon
      ;;
    'Skip for now')
      info 'Skipping cargo install. Release binary remains at target/release/lingclaw.'
      ;;
    *)
      warn "Unknown install choice: $choice"
      exit 1
      ;;
  esac
}

build_frontend() {
  local static_index="$ROOT_DIR/static/index.html"
  if ! ensure_node; then
    if [[ -f "$static_index" ]]; then
      warn "Using existing frontend bundle: $static_index"
      return
    fi
    warn 'Node.js / npm could not be prepared and static/index.html is missing.'
    exit 1
  fi
  info "Building frontend assets (Node.js $(node --version), npm $(npm --version))."
  (cd "$ROOT_DIR/frontend" && npm ci --silent && npm run build)
  info 'Frontend build complete: static/'
}

main() {
  ensure_rust
  install_build_deps
  build_frontend

  info 'Building LingClaw release binary.'
  cargo build --release
  info 'Build complete: target/release/lingclaw'

  PS3='Select the next step: '
  select choice in 'Install' 'Install-daemon' 'Skip for now'; do
    if [[ -n "${choice:-}" ]]; then
      run_install_choice "$choice"
      break
    fi
    warn 'Please choose a valid option.'
  done
}

main "$@"
