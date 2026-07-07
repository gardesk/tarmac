#!/usr/bin/env bash
# 00_smoke — the daemon starts, answers IPC, and registers real windows.
# If this fails, nothing downstream is meaningful.

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=e2e/lib.sh
source ./lib.sh

tarmac_start
close_all_textedit
open_windows 2

windows_json="$(tc get-windows)"
count="$(printf '%s' "$windows_json" | jq '.windows | length')"
assert_ge 2 "$count" "at least 2 windows registered"

ws_json="$(tc get-workspaces)"
assert_contains "$ws_json" '"active"' "get-workspaces returns an active workspace"

assert_summary
