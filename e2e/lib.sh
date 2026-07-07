#!/usr/bin/env bash
# e2e/lib.sh — shared helpers for the tarmac end-to-end harness.
#
# Two execution modes, selected by $TARMAC_E2E_MODE:
#   vm    (default) — drive tarmac inside an ephemeral tart VM cloned from a
#                     provisioned template. Full isolation; needs a Tahoe
#                     template with Accessibility granted (see provision.sh).
#   local          — drive a tarmac already running on this host. Fast
#                     iteration; requires the host to have granted the daemon
#                     Accessibility. No VM is touched.
#
# Sourced by run.sh and every tests/*.sh. Nothing here boots a VM on its own —
# run.sh owns lifecycle; the helpers just talk to whatever tarmac the mode
# points at.

set -euo pipefail

# ---- configuration knobs (env-overridable) --------------------------------

: "${TARMAC_E2E_MODE:=vm}"
# Provisioned template the ephemeral test VM is cloned from.
: "${TARMAC_E2E_TEMPLATE:=tarmac-e2e}"
# Name of the ephemeral clone. Unique-ish without relying on $RANDOM in a way
# that breaks reruns; run.sh may override.
: "${TARMAC_E2E_VM:=tarmac-e2e-run}"
# Credentials of the base image. cirruslabs images default to admin/admin.
: "${TARMAC_E2E_SSH_USER:=admin}"
: "${TARMAC_E2E_SSH_PASS:=admin}"
# Where files land inside the VM.
: "${TARMAC_E2E_REMOTE_HOME:=/Users/${TARMAC_E2E_SSH_USER}}"
# Socket tarmac listens on (kept explicit so host and VM agree).
: "${TARMAC_SOCKET:=/tmp/tarmac-e2e.sock}"
export TARMAC_SOCKET

# Populated by vm_boot(); the clone's IP. Exported so per-test subprocesses that
# re-source this file inherit it instead of resetting to empty. `:=` preserves
# any value already in the environment.
: "${E2E_VM_IP:=}"
export E2E_VM_IP

# ---- logging --------------------------------------------------------------

_c() { if [ -t 1 ]; then printf '\033[%sm' "$1"; fi; }
log()   { printf '%s%s%s\n' "$(_c '1;34')" "-- $*" "$(_c 0)"; }
ok()    { printf '%s%s%s\n' "$(_c '1;32')" "PASS $*" "$(_c 0)"; }
warn()  { printf '%s%s%s\n' "$(_c '1;33')" "WARN $*" "$(_c 0)" >&2; }
die()   { printf '%s%s%s\n' "$(_c '1;31')" "FAIL $*" "$(_c 0)" >&2; exit 1; }

# ---- assertion counters + primitives --------------------------------------

E2E_PASS=0
E2E_FAIL=0

assert_eq() { # want got [label]
  local want="$1" got="$2" label="${3:-}"
  if [ "$want" = "$got" ]; then
    E2E_PASS=$((E2E_PASS + 1))
    ok "${label:-assert_eq}: '$got'"
  else
    E2E_FAIL=$((E2E_FAIL + 1))
    warn "${label:-assert_eq}: want '$want' got '$got'"
  fi
}

assert_ge() { # threshold got [label] — numeric got >= threshold
  local thr="$1" got="$2" label="${3:-}"
  if awk "BEGIN{exit !($got >= $thr)}"; then
    E2E_PASS=$((E2E_PASS + 1))
    ok "${label:-assert_ge}: $got >= $thr"
  else
    E2E_FAIL=$((E2E_FAIL + 1))
    warn "${label:-assert_ge}: $got < $thr"
  fi
}

assert_contains() { # haystack needle [label]
  local hay="$1" needle="$2" label="${3:-}"
  if printf '%s' "$hay" | grep -qF -- "$needle"; then
    E2E_PASS=$((E2E_PASS + 1))
    ok "${label:-assert_contains}: found '$needle'"
  else
    E2E_FAIL=$((E2E_FAIL + 1))
    warn "${label:-assert_contains}: '$needle' not in output"
  fi
}

# Summarize and set the shell exit status. Call at the end of a test file.
assert_summary() {
  log "results: $E2E_PASS passed, $E2E_FAIL failed"
  [ "$E2E_FAIL" -eq 0 ]
}

# ---- ssh / scp plumbing (vm mode) -----------------------------------------

_ssh_opts=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
           -o LogLevel=ERROR -o ConnectTimeout=10)

vm_ssh() { # run a command inside the VM, stdout passed through
  sshpass -p "$TARMAC_E2E_SSH_PASS" \
    ssh "${_ssh_opts[@]}" \
    "${TARMAC_E2E_SSH_USER}@${E2E_VM_IP}" "$@"
}

vm_scp() { # vm_scp <local> <remote-abs-path>
  sshpass -p "$TARMAC_E2E_SSH_PASS" \
    scp "${_ssh_opts[@]}" "$1" \
    "${TARMAC_E2E_SSH_USER}@${E2E_VM_IP}:$2"
}

# ---- VM lifecycle (vm mode) -----------------------------------------------

vm_clone() {
  log "cloning $TARMAC_E2E_TEMPLATE -> $TARMAC_E2E_VM"
  tart list --format json | jq -e --arg n "$TARMAC_E2E_TEMPLATE" \
    '.[] | select(.Name == $n)' >/dev/null \
    || die "template '$TARMAC_E2E_TEMPLATE' not found — run e2e/provision.sh first"
  vm_delete >/dev/null 2>&1 || true
  tart clone "$TARMAC_E2E_TEMPLATE" "$TARMAC_E2E_VM"
}

vm_boot() {
  log "booting $TARMAC_E2E_VM"
  tart run --no-graphics "$TARMAC_E2E_VM" >/dev/null 2>&1 &
  E2E_VM_PID=$!
  # Wait for an IP, then for SSH to answer.
  local ip="" tries=0
  while [ -z "$ip" ] && [ "$tries" -lt 60 ]; do
    ip="$(tart ip "$TARMAC_E2E_VM" 2>/dev/null || true)"
    [ -z "$ip" ] && { sleep 2; tries=$((tries + 1)); }
  done
  [ -n "$ip" ] || die "VM never reported an IP"
  E2E_VM_IP="$ip"
  export E2E_VM_IP
  log "VM IP: $E2E_VM_IP — waiting for ssh"
  tries=0
  while ! vm_ssh true >/dev/null 2>&1; do
    sleep 2; tries=$((tries + 1))
    [ "$tries" -lt 60 ] || die "ssh never came up"
  done
  ok "VM reachable"
}

vm_delete() {
  tart stop "$TARMAC_E2E_VM" >/dev/null 2>&1 || true
  tart delete "$TARMAC_E2E_VM" >/dev/null 2>&1 || true
}

# ---- tarmac control (mode-agnostic) ---------------------------------------
#
# tc  — run tarmacctl against the daemon under test.
# tarmac_start / tarmac_stop / tarmac_kill — daemon lifecycle.
# In local mode these act on the host; in vm mode, inside the clone.

_run() { # execute a command in the active mode
  if [ "$TARMAC_E2E_MODE" = local ]; then
    /usr/bin/env bash -c "$*"
  else
    vm_ssh "$*"
  fi
}

tc() { # tarmacctl passthrough, e.g. tc get-windows
  _run "TARMAC_SOCKET=$TARMAC_SOCKET ${TARMAC_E2E_BIN_DIR:+$TARMAC_E2E_BIN_DIR/}tarmacctl $*"
}

# Launch the daemon detached with a file log; returns once IPC answers.
tarmac_start() {
  log "starting tarmac"
  _run "TARMAC_SOCKET=$TARMAC_SOCKET RUST_LOG=info \
        nohup ${TARMAC_E2E_BIN_DIR:+$TARMAC_E2E_BIN_DIR/}tarmac \
        >/tmp/tarmac-e2e.log 2>&1 & echo started"
  local tries=0
  while ! tc get-workspaces >/dev/null 2>&1; do
    sleep 1; tries=$((tries + 1))
    [ "$tries" -lt 30 ] || die "tarmac IPC never came up (see /tmp/tarmac-e2e.log)"
  done
  ok "tarmac up"
}

# Graceful stop: SIGTERM so the shutdown path (session flush + window restore)
# runs. This is the path restart-poisoning and session-restore tests exercise.
tarmac_stop() {
  log "stopping tarmac (SIGTERM)"
  _run "pkill -TERM -x tarmac || true"
  sleep 3
}

# Ungraceful kill: SIGKILL, bypassing the shutdown flush. Used to prove the
# periodic-persist safety net and off-screen-discovery hardening.
tarmac_kill() {
  log "killing tarmac (SIGKILL)"
  _run "pkill -KILL -x tarmac || true"
  sleep 2
}

# Open n throwaway windows via TextEdit documents (deterministic, always
# manageable). Waits for the daemon to register them.
open_windows() { # open_windows <n>
  local n="$1" i
  for i in $(seq 1 "$n"); do
    _run "osascript -e 'tell application \"TextEdit\" to make new document' >/dev/null 2>&1 || true"
    sleep 1
  done
  sleep 2
}

close_all_textedit() {
  _run "osascript -e 'tell application \"TextEdit\" to quit saving no' >/dev/null 2>&1 || true"
  sleep 2
}
