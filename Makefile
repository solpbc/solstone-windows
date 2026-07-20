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

.PHONY: install rust-toolchain preflight-toolchain preflight-cargo-deny build test ui-test \
	        test-scripts ci audit contract purity-check check-observer-contract package publish publish-r2 \
	        publish-winget publish-scoop publish-packages check-channels \
	        pull-releases require-win-remote-host sync-win-host win-host-ci \
	        smoke screenshots journal-live help

help:
	@echo "verbs: install rust-toolchain build test ci audit contract purity-check check-observer-contract package publish smoke screenshots journal-live run clean"
	@echo "release: package (box) -> publish (box) -> pull-releases -> publish-r2 -> publish-packages"
	@echo "ci = local fast checks + the remote Windows build/test; needs WIN_REMOTE_HOST=user@host"

# Local dev-tooling setup. The Rust/MSVC toolchain is remote (see win-host-ci);
# locally we only set up the UI's JS deps when present. Run by the hopper mill at
# lode start.
install:
	@if [ -f ui/package.json ]; then npm --prefix ui install; else echo "no local tooling to install"; fi

rust-toolchain:
	@version=$$(sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*$$/\1/p' rust-toolchain.toml | sed -n '1p'); \
	  test -n "$$version" || { echo "ERROR: unable to read rust-toolchain.toml channel" >&2; exit 1; }; \
	  rustup toolchain install "$$version" --profile minimal --component rustfmt --component clippy --target x86_64-pc-windows-msvc

preflight-toolchain:
	@sh scripts/preflight-toolchain.sh

preflight-cargo-deny:
	@CARGO="$(CARGO)" sh scripts/preflight-cargo-deny.sh

# Build the webview bundle + the binary. The webview is built FIRST: Tauri embeds
# ui/dist into the exe at cargo-compile time, so building it after would embed a
# stale bundle.
build: preflight-toolchain
	npm --prefix ui run build
	$(CARGO) build --locked -p $(TAURI_BIN) --features custom-protocol

# Local cross-platform tests (pure tier + capture-engine), host-testable, no live
# target. The windows-only crates test remotely via win-host-ci.
test: preflight-toolchain
	$(CARGO) test --locked --workspace $(REMOTE_CRATES)

# The host-testable shell publish-name contract check on the Linux mill.
test-scripts:
	sh scripts/lib/artifact-names.test.sh
	sh scripts/lib/deterministic-gates.test.sh

# UI unit tests (vitest+jsdom) on the Linux mill. Reinstall first so the new
# vitest/jsdom devDeps (added after the lode-start `npm install`) are present.
ui-test:
	npm --prefix ui install
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
	@echo "local offline observer-contract structural/behavioral evidence"
	CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- observer-contract check
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p xtask observer_contract
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p observer-pl observer_contract_authority
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p pl-transport-win observer_contract_authority
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p pl-transport-win --test transport_round_trip

# Build a RELEASE binary + webview, then pack a Velopack release into Releases/.
# Release (not the debug `build`) so the tray app is windowless — the
# `windows_subsystem="windows"` attribute is release-only. The webview is built
# first (embedded at cargo-compile time). The .ps1 consumes target/release/ and
# does not rebuild. Unsigned now; the $SignTemplate seam in scripts/package.ps1 is
# empty until the cert lands.
package: preflight-toolchain
	npm --prefix ui run build
	# --features custom-protocol: serve the embedded ui/dist, not the Vite devUrl.
	# Without it the shipped Settings/About load "localhost refused to connect"
	# (cargo tauri build sets it automatically; a plain cargo build does not).
	$(CARGO) build --locked -p $(TAURI_BIN) --release --features custom-protocol
	$(PWSH) -File scripts/package.ps1

# Upload the Releases/ dir to GitHub Releases = the REQUIRED source-hygiene mirror
# (tagged v<version> release + artifacts + the CHANGELOG ## [<version>] notes, same
# notes as the R2 feed). Every signed release publishes to BOTH R2 (publish-r2) and
# GitHub (this) -- never skipped. Runs on the RELEASE HOST (where `gh` is authed +
# Releases/ was pulled), same posture as publish-r2 -- the build box has no `gh`.
publish:
	sh scripts/publish-gh.sh Releases

# Upload the Releases/ dir to the R2 update feed at
# updates.solstone.app/solstone-windows/ -- the PRIMARY auto-update channel the
# in-app updater fetches, feed-last. Runs on the RELEASE HOST (where wrangler
# holds the Cloudflare R2 auth), not the build box -- keeps Cloudflare creds off
# the signing box. Pack on the box, `make pull-releases`, then this.
publish-r2:
	sh scripts/publish-r2.sh Releases

# Refresh the package-manager channels for a PUBLISHED release. Run on the RELEASE
# HOST after `make publish` (the GitHub release + assets must exist; the manifests
# point at and are hashed over those assets). Both ride the existing signed
# artifacts -- no rebuild. VERSION defaults to the workspace version; override with
# `make publish-packages VERSION=x.y.z`. See packaging/DISTRIBUTION.md.
#   winget -> a version-update PR to microsoft/winget-pkgs (needs komac).
#   scoop  -> bump version+hash in solpbc/scoop-solstone.
publish-winget:
	sh scripts/publish-winget.sh $(VERSION)

publish-scoop:
	sh scripts/publish-scoop.sh $(VERSION)

publish-packages: publish-winget publish-scoop

# Assert the package-manager channels actually carry the current release. Read-only.
# `publish-packages` is an operator step that fails quietly -- winget silently drifted
# ten releases behind before anyone noticed. Run this after a release.
check-channels:
	sh scripts/check-channels.sh $(VERSION)

# Pull the box's packed Releases/ to the release host so publish-r2 can upload it.
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
