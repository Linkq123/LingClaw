#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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

main() {
  ensure_rust
  install_build_deps

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