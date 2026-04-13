#!/usr/bin/env bash
set -euo pipefail

REPO="zhyongrui/repo-auto-puller"
INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/repo-auto-puller"
STATE_DIR="${HOME}/.local/state/repo-auto-puller"
SYSTEMD_DIR="${HOME}/.config/systemd/user"
CONFIG_PATH="${CONFIG_DIR}/config.toml"
SERVICE_PATH="${SYSTEMD_DIR}/repo-auto-puller.service"

os="$(uname -s)"
arch="$(uname -m)"

case "${os}" in
  Linux) target_os="unknown-linux-gnu" ;;
  Darwin) target_os="apple-darwin" ;;
  *)
    echo "Unsupported OS: ${os}" >&2
    exit 1
    ;;
esac

case "${arch}" in
  x86_64|amd64) target_arch="x86_64" ;;
  arm64|aarch64) target_arch="aarch64" ;;
  *)
    echo "Unsupported architecture: ${arch}" >&2
    exit 1
    ;;
esac

asset="repo-auto-puller-${target_arch}-${target_os}.tar.gz"
tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "${tmpdir}"
}
trap cleanup EXIT

mkdir -p "${INSTALL_DIR}" "${CONFIG_DIR}" "${STATE_DIR}" "${SYSTEMD_DIR}"

download_url="https://github.com/${REPO}/releases/latest/download/${asset}"
echo "Downloading ${download_url}"
curl -fsSL "${download_url}" -o "${tmpdir}/${asset}"
tar -xzf "${tmpdir}/${asset}" -C "${tmpdir}"
install -m 0755 "${tmpdir}/repo-auto-puller" "${INSTALL_DIR}/repo-auto-puller"

if [[ ! -f "${CONFIG_PATH}" ]]; then
  cat > "${CONFIG_PATH}" <<'EOF'
[defaults]
log_file = "~/.local/state/repo-auto-puller/repo-auto-puller.log"
verbose = false

[[repositories]]
name = "my-repo"
path = "/path/to/your/repo"
interval_seconds = 60
enabled = true
dry_run = false
EOF
  echo "Wrote example config to ${CONFIG_PATH}"
else
  echo "Keeping existing config at ${CONFIG_PATH}"
fi

cat > "${SERVICE_PATH}" <<'EOF'
[Unit]
Description=Repo Auto Puller
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=%h/.local/bin/repo-auto-puller --config %h/.config/repo-auto-puller/config.toml
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
EOF

echo
echo "Installed repo-auto-puller to ${INSTALL_DIR}/repo-auto-puller"
echo "Config file: ${CONFIG_PATH}"
echo "Service file: ${SERVICE_PATH}"
echo
echo "Next steps:"
echo "1. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} init --repo-path /path/to/your/repo --name my-repo"
echo "2. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} check-config"
echo "3. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} install-service --enable --start"
echo "4. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} status --repo my-repo"
