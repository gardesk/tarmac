# tarmac e2e harness (FW-9)

End-to-end tests that run the **real** `tarmac` daemon, open real windows, and
assert observable behaviour through `tarmacctl`. They cover the restart-hygiene
and hide/restore paths that unit tests can't reach because those paths depend on
the live Accessibility API and WindowServer.

## What it checks

| test | guards |
|------|--------|
| `00_smoke` | daemon starts, answers IPC, registers windows |
| `10_session_restore` | #19 — windows return to their workspace across a graceful restart |
| `20_restart_poisoning` | FW-1 — a floated+parked window survives a SIGKILL restart with sane geometry (not stuck off-screen, not inflated) |
| `30_ax_clamp` | hide/show — a workspace-hidden window is parked off-screen and fully restored on switch-back |

## Requirements

- [`tart`](https://tart.run) (tested with 2.32.1), `jq`, `sshpass`.
- A provisioned VM template. **The base image must be the macOS version tarmac
  targets (Tahoe 15.0+)** — SkyLight/AX behaviour differs across releases, so a
  Sonoma/Sequoia base only smoke-tests the harness plumbing, not real WM
  behaviour.
- The guest needs **SIP disabled** so `provision.sh` can grant Accessibility by
  writing the TCC database directly (non-interactive). This is why it runs in a
  throwaway VM and never on a real machine.

## Usage

```bash
# one-time: build the tarmac-e2e template from a base image
TARMAC_E2E_BASE=ghcr.io/cirruslabs/macos-sequoia-base:latest make e2e-provision

# run the full suite in an ephemeral clone
make e2e

# run one test, keep the VM afterwards for inspection
./e2e/run.sh --keep 20_restart

# fast iteration without a VM (drives a second daemon on this host)
make e2e-local
```

### Modes

- **vm** (default): clones the template per run, pushes the freshly-built release
  binary + `config/init.lua`, runs the tests over SSH, tears the clone down.
  Full isolation.
- **local** (`--local`): drives a `tarmac` on the host via a private socket
  (`/tmp/tarmac-e2e.sock`) and `TARMAC_CONFIG=e2e/config/init.lua`. Fast, but it
  will fight a live WM for the display — run it when no other tarmac is active.

## CI

`.github/workflows/e2e.yml` runs the suite on a **self-hosted** runner labelled
`[self-hosted, macos, tart]` — GitHub-hosted macOS runners are themselves VMs and
can't nest one. It is `workflow_dispatch` + manual; the fast `cargo test` suite
in `ci.yml` remains the per-PR gate.

## Structure

```
e2e/
  lib.sh            shared helpers: VM lifecycle, ssh/scp, tarmacctl, assertions
  provision.sh      build the tarmac-e2e template (TCC grant injection)
  run.sh            orchestrator (clone → push → run tests → teardown)
  config/init.lua   deterministic test config
  tests/*.sh        one file per scenario; each sources lib.sh and self-asserts
```

Each `tests/*.sh` is independent: it starts the daemon, does its work, and calls
`assert_summary` (non-zero exit on any failed assertion). `run.sh` aggregates by
exit code.
