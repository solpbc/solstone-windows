# solstone-windows — agent-native build verbs.
#
# One verb per intent. The agent invokes a verb; it never hand-chains
# `cargo build` -> `vpk pack` -> `gh release`. Windows-only steps live in
# scripts/*.ps1 so they are lockdown-able outside the agent's normal write scope.
#
# Releases are operator-driven from a known build box. There is no GitHub Actions
# path and .github/workflows/ does not exist — by policy, permanently.

SHELL := /bin/sh
PWSH ?= pwsh
CARGO ?= cargo
TAURI_BIN := solstone-windows-app

# Remote build-host targets (reserved). Set WIN_REMOTE_HOST=<host> to drive a
# Windows build box over SSH. The sync uses a dedicated remote tree so the
# destructive `--delete` can never clobber a working checkout on the build box.
WIN_REMOTE_HOST ?=
WIN_REMOTE_PROJECT ?= solstone-windows-host
RSYNC_EXCLUDES := --exclude .git --exclude target --exclude ui/dist --exclude ui/node_modules --exclude Releases

.PHONY: build test ci contract package publish smoke run clean \
        require-win-remote-host sync-win-host win-host-ci help

help:
	@echo "verbs: build test ci contract package publish smoke run clean"
	@echo "remote: WIN_REMOTE_HOST=<host> make win-host-ci"

# Build the binary + the webview bundle.
build:
	$(CARGO) build -p $(TAURI_BIN)
	npm --prefix ui run build

# Pure tier also runs here (host-testable); no live target.
test:
	$(CARGO) test --workspace

# The full gate. Contract drift is checked before the suite so it fails fast.
# (cargo fmt/clippy/deny are part of the gate where the toolchain provides them.)
ci:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets -- -D warnings
	$(CARGO) run -q -p xtask -- contract --check
	$(CARGO) test --workspace
	$(CARGO) deny check

# Regenerate automation-contract.json + the ui codegen; the operator commits.
contract:
	$(CARGO) run -q -p xtask -- contract

# Build then pack a Velopack release into Releases/. Unsigned now; the
# $SignTemplate seam in scripts/package.ps1 is empty until the cert lands.
package: build
	$(PWSH) -File scripts/package.ps1

# Upload the Releases/ dir to GitHub Releases = the monotonic update feed.
publish:
	$(PWSH) -File scripts/publish.ps1

# Register + fire the Session-1 scheduled-task FlaUI smoke against the installed
# app; poll health to `observing`. Live target — run on the build box.
smoke:
	$(PWSH) -File scripts/smoke.ps1

# Launch from the tree and tail the logs.
run:
	$(PWSH) -File scripts/run.ps1

clean:
	$(CARGO) clean
	rm -rf ui/dist Releases

# ── Remote build host (reserved) ─────────────────────────────────────────────
require-win-remote-host:
	@test -n "$(WIN_REMOTE_HOST)" || (echo "Set WIN_REMOTE_HOST=<host>" >&2; exit 2)

sync-win-host: require-win-remote-host
	ssh $(WIN_REMOTE_HOST) 'mkdir -p $(WIN_REMOTE_PROJECT)'
	rsync -az --delete $(RSYNC_EXCLUDES) ./ $(WIN_REMOTE_HOST):$(WIN_REMOTE_PROJECT)/

win-host-ci: sync-win-host
	ssh $(WIN_REMOTE_HOST) 'cd $(WIN_REMOTE_PROJECT) && make ci'
