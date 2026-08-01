#!/usr/bin/env bash
# QNC v2 — jedan ulaz za test (macOS / Linux)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
exec "${ROOT}/scripts/test_host.sh"
