#!/usr/bin/env bash
# e2e/provision.sh — build the reusable `tarmac-e2e` VM template.
#
# One-time (or occasional) setup. Clones a base macOS image, grants the tarmac
# binary Accessibility by writing the user TCC database directly (requires SIP
# disabled in the guest), then leaves a stopped template that run.sh clones per
# run.
#
#   TARMAC_E2E_BASE=ghcr.io/cirruslabs/macos-sequoia-base:latest ./e2e/provision.sh
#
# IMPORTANT: the base image must be the macOS version tarmac targets (Tahoe
# 15.0+). The SkyLight/AX behaviour tarmac depends on differs across releases,
# so a Sonoma base will build and boot but is only useful for smoke-testing the
# harness plumbing, not for asserting real WM behaviour. TCC's schema also
# shifts between releases; the INSERT below is known-good for the Sequoia/Tahoe
# era and may need adjustment on older images.

set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=e2e/lib.sh
source ./lib.sh

: "${TARMAC_E2E_BASE:=ghcr.io/cirruslabs/macos-sequoia-base:latest}"

log "provisioning template '$TARMAC_E2E_TEMPLATE' from base '$TARMAC_E2E_BASE'"

if tart list --format json | jq -e --arg n "$TARMAC_E2E_TEMPLATE" \
     '.[] | select(.Name == $n)' >/dev/null; then
  warn "template already exists; delete it first to rebuild:"
  warn "  tart delete $TARMAC_E2E_TEMPLATE"
  exit 0
fi

log "pulling/cloning base image (first run may take a while)"
tart clone "$TARMAC_E2E_BASE" "$TARMAC_E2E_TEMPLATE"

# Boot the template itself so we can inject the TCC grant, then stop it.
TARMAC_E2E_VM="$TARMAC_E2E_TEMPLATE"
vm_boot

log "granting Accessibility to tarmac via TCC (SIP must be disabled in guest)"
# The tarmac binary's designated requirement isn't stable across rebuilds, so we
# grant by client path with a NULL csreq and auth_value=2 (allowed). This is the
# permissive form intended for a throwaway test guest, never a real machine.
TCC_DB="${TARMAC_E2E_REMOTE_HOME}/Library/Application Support/com.apple.TCC/TCC.db"
BIN_PATH="${TARMAC_E2E_REMOTE_HOME}/bin/tarmac"
vm_ssh "sqlite3 '$TCC_DB' \"INSERT OR REPLACE INTO access \
  (service, client, client_type, auth_value, auth_reason, auth_version, \
   csreq, policy_id, indirect_object_identifier_type, \
   indirect_object_identifier, indirect_object_code_identity, flags, \
   last_modified) \
  VALUES('kTCCServiceAccessibility', '$BIN_PATH', 1, 2, 4, 1, \
   NULL, NULL, 0, 'UNUSED', NULL, 0, strftime('%s','now'));\"" \
  || warn "TCC insert failed — the guest may need SIP off or a schema tweak"

vm_ssh "mkdir -p '${TARMAC_E2E_REMOTE_HOME}/bin' '${TARMAC_E2E_REMOTE_HOME}/.config/tarmac'"

log "stopping template"
tart stop "$TARMAC_E2E_TEMPLATE" >/dev/null 2>&1 || true

ok "template '$TARMAC_E2E_TEMPLATE' ready — run 'make e2e' to use it"
