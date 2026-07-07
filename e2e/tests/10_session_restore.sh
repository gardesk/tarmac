#!/usr/bin/env bash
# 10_session_restore — the #19 feature end-to-end. Spread windows across
# workspaces, quit gracefully (SIGTERM flushes the session file), restart, and
# assert each window returned to the workspace it was on.

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=e2e/lib.sh
source ./lib.sh

tarmac_start
close_all_textedit
open_windows 3

# Move the focused window to workspace 2, another to workspace 3, leave one on 1.
tc focus left  >/dev/null 2>&1 || true
tc move-to-workspace 2 >/dev/null
tc workspace 1 >/dev/null
tc focus right >/dev/null 2>&1 || true
tc move-to-workspace 3 >/dev/null
tc workspace 1 >/dev/null

# Capture the pre-restart placement: map of window id -> workspace.
before="$(tc get-windows | jq -c '[.windows[] | {id, workspace}] | sort_by(.id)')"
log "placement before restart: $before"

# Force the periodic persist to have run, then quit gracefully so the shutdown
# flush writes the definitive session file.
sleep 1
tarmac_stop

tarmac_start
sleep 3  # let discovery + restore settle

after="$(tc get-windows | jq -c '[.windows[] | {id, workspace}] | sort_by(.id)')"
log "placement after restart: $after"

# Window ids are stable across a daemon restart (the apps did not restart), so
# the id->workspace mapping must be preserved.
assert_eq "$before" "$after" "workspace placement preserved across restart"

# And the workspaces that had windows before must still be non-empty.
ws2="$(tc get-windows | jq '[.windows[] | select(.workspace=="2")] | length')"
ws3="$(tc get-windows | jq '[.windows[] | select(.workspace=="3")] | length')"
assert_ge 1 "$ws2" "workspace 2 restored a window"
assert_ge 1 "$ws3" "workspace 3 restored a window"

assert_summary
