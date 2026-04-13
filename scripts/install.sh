#!/usr/bin/env bash
set -euo pipefail

REPO="zhyongrui/repo-auto-puller"
INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.config/repo-auto-puller"
STATE_DIR="${HOME}/.local/state/repo-auto-puller"
CONFIG_PATH="${CONFIG_DIR}/config.toml"

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

mkdir -p "${INSTALL_DIR}" "${CONFIG_DIR}" "${STATE_DIR}"

download_url="https://github.com/${REPO}/releases/latest/download/${asset}"
echo "Downloading ${download_url}"
downloaded=false
if curl --fail --show-error --location --retry 5 --retry-delay 2 --retry-all-errors "${download_url}" -o "${tmpdir}/${asset}"; then
  downloaded=true
elif command -v gh >/dev/null 2>&1; then
  echo "curl download failed, retrying with gh release download"
  gh release download -R "${REPO}" --pattern "${asset}" --output "${tmpdir}/${asset}" --clobber
  downloaded=true
fi

if [[ "${downloaded}" != "true" ]]; then
  echo "Failed to download ${asset}" >&2
  echo "Try again later, or install GitHub CLI and rerun this script for an automatic fallback." >&2
  exit 1
fi

tar -xzf "${tmpdir}/${asset}" -C "${tmpdir}"
install -m 0755 "${tmpdir}/repo-auto-puller" "${INSTALL_DIR}/repo-auto-puller"

if [[ ! -f "${CONFIG_PATH}" ]]; then
  cat > "${CONFIG_PATH}" <<'EOF'
config_version = 1

[defaults]
log_file = "~/.local/state/repo-auto-puller/repo-auto-puller.log"
state_file = "~/.local/state/repo-auto-puller/status.json"
history_file = "~/.local/state/repo-auto-puller/history.jsonl"
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

echo
echo "Installed repo-auto-puller to ${INSTALL_DIR}/repo-auto-puller"
echo "Config file: ${CONFIG_PATH}"
echo
echo "Next steps:"
echo "1. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} init --repo-path /path/to/your/repo --name my-repo"
echo "2. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} check-config"
echo "3. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} install-service --enable --start"
echo "4. Run: ${INSTALL_DIR}/repo-auto-puller --config ${CONFIG_PATH} status --repo my-repo"
