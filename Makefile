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
GIT ?= git
SCP ?= scp
SSH ?= ssh
TAURI_BIN := solstone-windows-app

# Windows-only crates: their real build/test/lint runs on the Windows box
# (windows-rs + MSVC) via win-host-ci. Excluded from the local fast checks,
# which cover the cross-platform crates only (pure tier + capture-engine).
REMOTE_CRATES := --exclude $(TAURI_BIN) --exclude capture-wgc --exclude capture-wasapi --exclude platform-win --exclude capture-screen-encode

# Remote build host. The Windows-only toolchain (Rust-MSVC, windows-rs, Tauri,
# Velopack, FlaUI) builds on a Windows build box; code + git stay on the dev host
# and only the build/test runs remotely, streamed back. Transport is git: a bundle
# of the exact working tree (committed or not) carried by scp and fetched on the
# box — no rsync, no remote `make`, no POSIX assumptions about the box. The box
# bootstrap (C:\sol\sw-ci.cmd) fetches the bundle, hard-checks-out, and runs
# scripts/win-ci.cmd. WIN_REMOTE_HOST is supplied by the build environment, never
# committed (public hygiene): WIN_REMOTE_HOST=user@host make win-host-ci.
WIN_REMOTE_HOST ?=
WIN_SCP ?= scp -o ControlMaster=auto -o ControlPath=/tmp/sw-%r@%h:%p -o ControlPersist=60s

.PHONY: install ui-deps-update rust-toolchain preflight-toolchain preflight-cargo-deny \
	        provision-cargo-deny preflight-release-tools build test ui-test \
	        test-scripts ci audit contract purity-check check-observer-contract package publish publish-r2 \
	        publish-winget publish-scoop publish-packages check-channels \
	        pull-releases require-win-remote-host sync-win-host win-host-ci \
	        smoke screenshots journal-live help

help:
	@echo "verbs: install ui-deps-update rust-toolchain provision-cargo-deny build test ci audit contract purity-check check-observer-contract package smoke screenshots journal-live run clean"
	@echo "release: package constructs Releases/; direct publish targets are locked pending the aggregate provenance publisher"
	@echo "ci = local fast checks + the remote Windows build/test; needs WIN_REMOTE_HOST=user@host"

# Local dev-tooling setup. The Rust/MSVC toolchain is remote (see win-host-ci);
# locally we only set up the UI's JS deps when present. Run by the hopper mill at
# lode start.
install:
	@if [ -f ui/package.json ]; then npm --prefix ui ci; else echo "no local tooling to install"; fi

# The only dependency-update path. Network is permitted; review and commit the
# resulting ui/package-lock.json diff before using deterministic consumers.
ui-deps-update:
	npm --prefix ui install

rust-toolchain:
	@version=$$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*$$/\1/p' rust-toolchain.toml | sed -n '1p'); \
	  test -n "$$version" || { echo "ERROR: unable to read rust-toolchain.toml channel" >&2; exit 1; }; \
	  rustup toolchain install "$$version" --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-msvc

preflight-toolchain:
	@sh scripts/preflight-toolchain.sh

preflight-cargo-deny:
	@CARGO="$(CARGO)" sh scripts/preflight-cargo-deny.sh

provision-cargo-deny:
	cargo install cargo-deny --version 0.20.2 --locked

# Windows build-box release-tool observation only: no credentials or network.
preflight-release-tools:
	$(PWSH) -NoProfile -File packaging/preflight-release-tools.ps1

# Build the webview bundle + the binary. The webview is built FIRST: Tauri embeds
# ui/dist into the exe at cargo-compile time, so building it after would embed a
# stale bundle.
build: preflight-toolchain
	npm --prefix ui ci --offline
	npm --prefix ui run build
	$(CARGO) build --locked -p $(TAURI_BIN) --features custom-protocol

# Local cross-platform tests (pure tier + capture-engine), host-testable, no live
# target. The windows-only crates test remotely via win-host-ci.
test: preflight-toolchain
	$(CARGO) test --locked --workspace $(REMOTE_CRATES)

# Host-testable deterministic/package/publication policy checks on the Linux mill.
test-scripts:
	sh scripts/lib/deterministic-gates.test.sh
	sh scripts/lib/publication-guard.test.sh
	sh scripts/lib/make-package-ordering.test.sh

# UI unit tests (vitest+jsdom) on the Linux mill. Materialize only the committed
# graph from the warmed cache; fail if the cache is incomplete.
ui-test:
	npm --prefix ui ci --offline
	npm --prefix ui run test

# The one CI surface for the engineer: cheap, host-independent checks run locally
# and fail fast, then the real Windows build + test runs on the build box. One
# flow. fmt/deny/contract/purity are host-independent; clippy + test cover the
# cross-platform crates (pure tier + capture-engine). The windows-only crates are
# built and tested remotely by win-host-ci.
ci: preflight-toolchain preflight-cargo-deny
	$(CARGO) fmt --all --check
	$(CARGO) clippy --locked --workspace $(REMOTE_CRATES) --all-targets -- -D warnings
	$(CARGO) run --locked -q -p xtask -- contract --check
	$(CARGO) run --locked -q -p xtask -- purity-check
	$(MAKE) check-observer-contract
	$(CARGO) test --locked --workspace $(REMOTE_CRATES)
	$(CARGO) deny --offline --locked check bans licenses sources
	$(MAKE) ui-test
	$(MAKE) test-scripts
	$(MAKE) win-host-ci

# Refresh the RustSec advisory database, then check it against the locked graph.
# This networked freshness check is deliberately separate from deterministic CI.
audit: preflight-toolchain preflight-cargo-deny
	@$(CARGO) deny fetch db || { echo "ERROR: RustSec advisory database refresh failed; no current advisory result was produced." >&2; exit 1; }
	$(CARGO) deny --locked check advisories

# Regenerate automation-contract.json + the ui codegen; the operator commits.
contract: preflight-toolchain
	$(CARGO) run --locked -q -p xtask -- contract

# Structural gate: the `windows` family must never reach the pure tier
# (AGENTS.md §Source Layout). `--target all` makes target-gated leaks visible on any host.
purity-check: preflight-toolchain
	$(CARGO) run --locked -q -p xtask -- purity-check

# Local offline observer-client contract structural/behavioral evidence only.
check-observer-contract: preflight-toolchain
	@echo "local offline observer-client authority bundle structural/behavioral evidence"
	CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- observer-contract check
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p xtask observer_contract
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p observer-pl observer_contract_authority
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p pl-transport-win observer_contract_authority
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p pl-transport-win --test transport_round_trip

# Gate, build a RELEASE binary + webview, then pack into Releases/.
# Release (not the debug `build`) so the tray app is windowless — the
# `windows_subsystem="windows"` attribute is release-only. The webview is built
# first (embedded at cargo-compile time). The .ps1 consumes target/release/ and
# does not rebuild. The explicit ordering is duplicated inside package.ps1 for
# direct-invocation defense in depth.
package:
	@set -eu; \
	  sign_arg=; \
	  if [ -n "$${SOLSTONE_SIGN:-}" ]; then sign_arg=-Sign; fi; \
	  selection_json=$$($(PWSH) -NoProfile -File packaging/preflight-release-tools.ps1 $$sign_arg); \
	  test -n "$$selection_json"; \
	  cargo_path=$$(SOLSTONE_RELEASE_SELECTION_JSON="$$selection_json" $(PWSH) -NoProfile -Command '($$env:SOLSTONE_RELEASE_SELECTION_JSON | ConvertFrom-Json).tools.cargo.path'); \
	  npm_path=$$(SOLSTONE_RELEASE_SELECTION_JSON="$$selection_json" $(PWSH) -NoProfile -Command '($$env:SOLSTONE_RELEASE_SELECTION_JSON | ConvertFrom-Json).tools.npm.path'); \
	  powershell_path=$$(SOLSTONE_RELEASE_SELECTION_JSON="$$selection_json" $(PWSH) -NoProfile -Command '($$env:SOLSTONE_RELEASE_SELECTION_JSON | ConvertFrom-Json).tools.powershell.path'); \
	  test -n "$$cargo_path"; test -n "$$npm_path"; test -n "$$powershell_path"; \
	  SOLSTONE_VERSION_GATE_CARGO="$$cargo_path" "$$cargo_path" run --locked -q -p xtask -- version-gate; \
	  "$$powershell_path" -NoProfile -File packaging/lock-guard.ps1; \
	  "$$npm_path" --prefix ui ci --offline; \
	  "$$npm_path" --prefix ui run build; \
	  "$$cargo_path" build --locked -p $(TAURI_BIN) --release --features custom-protocol; \
	  if [ -n "$$sign_arg" ]; then \
	    "$$powershell_path" -NoProfile -File scripts/package.ps1 -Sign; \
	  else \
	    "$$powershell_path" -NoProfile -File scripts/package.ps1; \
	  fi

# Direct publication is fail-closed. These entry points remain visible while
# publication ownership moves to the aggregate provenance publisher.
publish:
	sh scripts/publish-gh.sh

publish-r2:
	sh scripts/publish-r2.sh

publish-winget:
	sh scripts/publish-winget.sh

publish-scoop:
	sh scripts/publish-scoop.sh

publish-packages: publish-winget publish-scoop

# Assert the package-manager channels carry the metadata-derived current release.
# Read-only; it never publishes or repairs channel drift.
check-channels:
	sh scripts/check-channels.sh

# Pull the box's packed Releases/ for a controlled aggregate workflow.
# The box checks the working tree out under ~/swbuild (sync-win-host's bundle).
pull-releases: require-win-remote-host
	rm -rf Releases
	$(WIN_SCP) -r $(WIN_REMOTE_HOST):swbuild/Releases Releases
	@echo "pulled Releases/ from $(WIN_REMOTE_HOST)"

# Launch the installed app in Session 1, then run the load-bearing health/render
# smoke directly from Session 0. Live target - run on the build box.
smoke:
	$(PWSH) -File scripts/smoke.ps1

# Capture Settings/About in Session 1. Live target - run on the build box.
screenshots:
	$(PWSH) -File scripts/screenshot.ps1

# Validate the native Journal window against a committed mock journal.
# Live target - VPE-run on the build box, not part of ci.
journal-live:
	$(PWSH) -File scripts/journal-window-live.ps1

# Launch from the tree and tail the logs.
run:
	$(PWSH) -File scripts/run.ps1

clean:
	$(CARGO) clean
	rm -rf ui/dist Releases

# ── Remote build host ─────────────────────────────────────────────────────────
require-win-remote-host:
	@test -n "$(WIN_REMOTE_HOST)" || (echo "Set WIN_REMOTE_HOST=user@host" >&2; exit 2)

# Bundle the exact working tree (incl. uncommitted) and ship it to the box.
# Internal transfer step; win-host-ci serializes runs, and its HEAD check rejects out-of-band bundle overwrites.
sync-win-host: require-win-remote-host
	@WIN_REMOTE_HOST="$(WIN_REMOTE_HOST)" GIT="$(GIT)" SCP="$(SCP)" sh scripts/sync-win-host.sh

# Run native preflights, build, tests, contract, and purity; accept only the exact transferred snapshot HEAD.
# The live FlaUI smoke + lifecycle matrix are operator-direct, not part of this.
win-host-ci: require-win-remote-host
	@WIN_REMOTE_HOST="$(WIN_REMOTE_HOST)" GIT="$(GIT)" SCP="$(SCP)" SSH="$(SSH)" sh scripts/win-host-ci.sh
