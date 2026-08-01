#!/usr/bin/env bash
# QNC multiplatform shell — Rust host (bez Pythona)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
HOST_DIR="${ROOT}/qnc-host"
export QNC_ROOT="${ROOT}"
export QNC_APP_VERSION="${QNC_APP_VERSION:-host-0.1}"
export QNC_API_PORT="${QNC_API_PORT:-8001}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "GREŠKA: instaliraj Rust — https://rustup.rs" >&2
  echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
  exit 1
fi

cd "${HOST_DIR}"
if [[ "${1:-}" == "build" ]]; then
  cargo build --release
  echo "Build: ${HOST_DIR}/target/release/qnc-host"
  exit 0
fi

if [[ ! -x "${HOST_DIR}/target/release/qnc-host" ]]; then
  echo "Prvi build (release)..."
  cargo build --release
fi

echo "=== QNC shell host ==="
echo "Root: ${ROOT}"
echo "URL:  http://127.0.0.1:${QNC_API_PORT}/app  (LAN: export QNC_BIND_HOST=0.0.0.0)"
exec "${HOST_DIR}/target/release/qnc-host"
