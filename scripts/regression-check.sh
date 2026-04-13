#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if ! rustup target list --installed | grep -qx 'x86_64-pc-windows-msvc'; then
  rustup target add x86_64-pc-windows-msvc
fi

cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash -n scripts/install.sh
cargo check --workspace --target x86_64-pc-windows-msvc
