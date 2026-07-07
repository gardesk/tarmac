# tarmac — convenience targets. The Rust build stays cargo-native; this is
# mostly the e2e harness entry points (see e2e/README.md).

CARGO ?= cargo

.PHONY: build release test clippy fmt e2e e2e-local e2e-provision e2e-clean

build:
	$(CARGO) build --workspace

release:
	$(CARGO) build --release --workspace

test:
	$(CARGO) test --workspace

clippy:
	$(CARGO) clippy --all-targets -- -D warnings -A clippy::too_many_arguments

fmt:
	$(CARGO) fmt --all

# One-time: build the reusable VM template (see e2e/provision.sh).
e2e-provision:
	./e2e/provision.sh

# Full suite in an ephemeral VM cloned from the template.
e2e:
	./e2e/run.sh

# Drive a tarmac on this host — fast iteration, no VM. Run only when no other
# tarmac owns the display.
e2e-local:
	./e2e/run.sh --local

# Remove the ephemeral run VM (leaves the template intact).
e2e-clean:
	tart delete tarmac-e2e-run 2>/dev/null || true
