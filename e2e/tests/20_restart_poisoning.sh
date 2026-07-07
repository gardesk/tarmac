#!/usr/bin/env bash
# 20_restart_poisoning — the FW-1 regression. A window floated and parked, then
# the daemon SIGKILLed (no shutdown restore) and restarted, must come back with
# sane geometry — not stuck at the off-screen park coordinates, not larger than
# its monitor. This is the geometry-poisoning class that made floats unusable.

set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=e2e/lib.sh
source ./lib.sh

tarmac_start
close_all_textedit
open_windows 2

# Float the focused window, then push it to the scratchpad so it is parked
# off-screen at kill time — the exact precondition FW-1 hardened against.
tc toggle-floating >/dev/null
sleep 1
tc move-to-special scratch >/dev/null
sleep 1

# Hard kill: bypass the graceful shutdown restore entirely.
tarmac_kill
tarmac_start
sleep 3

# Pull the scratchpad back into view and inspect its geometry.
tc toggle-special scratch >/dev/null
sleep 2

geo="$(tc get-windows | jq -c '[.windows[] | {id, x, y, width, height, floating}]')"
log "post-restart geometry: $geo"

# No window should sit at the park sentinel (-32000-ish) or otherwise far
# off-screen to the negative — that is the poisoned state.
parked="$(tc get-windows | jq '[.windows[] | select(.x < -10000 or .y < -10000)] | length')"
assert_eq 0 "$parked" "no window stuck at off-screen park coordinates"

# No window should exceed a sane maximum (monitors here are <= 8K wide/tall);
# a poisoned float historically inflated to absurd dimensions.
oversized="$(tc get-windows | jq '[.windows[] | select(.width > 8192 or .height > 8192)] | length')"
assert_eq 0 "$oversized" "no window inflated beyond monitor bounds"

# Positive-size sanity: every window has real dimensions.
degenerate="$(tc get-windows | jq '[.windows[] | select(.width <= 1 or .height <= 1)] | length')"
assert_eq 0 "$degenerate" "no zero/negative-size windows after restart"

assert_summary
