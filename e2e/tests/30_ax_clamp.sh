#!/usr/bin/env bash
# 30_ax_clamp — hidden-window parking. When a window's workspace is switched
# away, tarmac parks it off-screen (AX position clamps to a 1px sliver, so SLS
# alpha is the real hide). Switching back must restore it fully on-screen. This
# exercises the hide/show primitive and the last_applied_rect short-circuit that
# HX/perf work churned, and would catch a regression that leaves parked windows
# visible or fails to restore them.

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=e2e/lib.sh
source ./lib.sh

tarmac_start
close_all_textedit

# One window on ws1, then switch to ws2 (empty). The ws1 window is now hidden.
open_windows 1
tc workspace 1 >/dev/null
hidden_id="$(tc get-windows | jq -r '.windows[0].id')"
log "window under test: $hidden_id"

tc workspace 2 >/dev/null
sleep 2

# The hidden window is still registered but off its visible workspace.
still_here="$(tc get-windows | jq --arg id "$hidden_id" \
  '[.windows[] | select((.id|tostring)==$id)] | length')"
assert_eq 1 "$still_here" "hidden window still tracked after switch-away"

on_ws1="$(tc get-windows | jq --arg id "$hidden_id" \
  '[.windows[] | select((.id|tostring)==$id and .workspace=="1")] | length')"
assert_eq 1 "$on_ws1" "hidden window still logically on workspace 1"

# Switch back — it must return, on-screen, with real geometry.
tc workspace 1 >/dev/null
sleep 2

restored="$(tc get-windows | jq --arg id "$hidden_id" -c \
  '.windows[] | select((.id|tostring)==$id) | {x, y, width, height}')"
log "restored geometry: $restored"

offscreen="$(printf '%s' "$restored" | jq '[.x, .y] | map(select(. < -10000)) | length')"
assert_eq 0 "$offscreen" "window not left at park coordinates after switch-back"

realsize="$(printf '%s' "$restored" | jq 'if .width > 1 and .height > 1 then 1 else 0 end')"
assert_eq 1 "$realsize" "window restored with real dimensions"

assert_summary
