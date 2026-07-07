#!/usr/bin/env bash
# e2e/run.sh — orchestrate the tarmac end-to-end suite.
#
#   ./e2e/run.sh                 # vm mode: clone template, run all tests, teardown
#   ./e2e/run.sh --local         # drive a tarmac on THIS host (fast iteration)
#   ./e2e/run.sh --keep          # leave the VM running for inspection
#   ./e2e/run.sh 20_restart      # run only tests whose name matches the filter
#
# vm mode needs a provisioned template (see provision.sh). local mode spins up a
# second daemon on a private socket/config; run it when no other tarmac owns the
# display, or it will fight the live WM.

set -euo pipefail
cd "$(dirname "$0")"

KEEP=0
FILTER=""
for arg in "$@"; do
  case "$arg" in
    --local) export TARMAC_E2E_MODE=local ;;
    --keep)  KEEP=1 ;;
    --*)     echo "unknown flag: $arg" >&2; exit 2 ;;
    *)       FILTER="$arg" ;;
  esac
done

# shellcheck source=e2e/lib.sh
source ./lib.sh

REPO_ROOT="$(cd .. && pwd)"
CARGO="${CARGO:-cargo}"

log "building release binaries"
( cd "$REPO_ROOT" && "$CARGO" build --release -p tarmac -p tarmacctl >/dev/null )

cleanup() {
  tarmac_stop || true
  if [ "$TARMAC_E2E_MODE" = vm ] && [ "$KEEP" -eq 0 ]; then
    vm_delete
  fi
}
trap cleanup EXIT

if [ "$TARMAC_E2E_MODE" = local ]; then
  warn "local mode: this starts a second tarmac on $TARMAC_SOCKET"
  export TARMAC_E2E_BIN_DIR="$REPO_ROOT/target/release"
  export TARMAC_CONFIG="$PWD/config/init.lua"
else
  vm_clone
  vm_boot
  export TARMAC_E2E_BIN_DIR="$TARMAC_E2E_REMOTE_HOME/bin"
  log "pushing binaries + config into VM"
  vm_scp "$REPO_ROOT/target/release/tarmac"    "$TARMAC_E2E_BIN_DIR/tarmac"
  vm_scp "$REPO_ROOT/target/release/tarmacctl" "$TARMAC_E2E_BIN_DIR/tarmacctl"
  vm_scp "$PWD/config/init.lua" "$TARMAC_E2E_REMOTE_HOME/.config/tarmac/init.lua"
  vm_ssh "chmod +x '$TARMAC_E2E_BIN_DIR/tarmac' '$TARMAC_E2E_BIN_DIR/tarmacctl'"
  # Clear any session file from a previous run so tests start from a known state.
  vm_ssh "rm -f '$TARMAC_E2E_REMOTE_HOME/.config/tarmac/session.json'"
fi

total_fail=0
for test in tests/*.sh; do
  name="$(basename "$test" .sh)"
  [ -n "$FILTER" ] && [[ "$name" != *"$FILTER"* ]] && continue
  echo
  log "==== $name ===="
  if bash "$test"; then
    ok "$name"
  else
    warn "$name FAILED"
    total_fail=$((total_fail + 1))
  fi
done

echo
if [ "$total_fail" -eq 0 ]; then
  ok "e2e suite passed"
else
  die "$total_fail test file(s) failed"
fi
