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
WIN_BUNDLE ?= $(CURDIR)/target/sync.bundle
WIN_SSH ?= ssh -o ControlMaster=auto -o ControlPath=/tmp/sw-%r@%h:%p -o ControlPersist=60s
WIN_SCP ?= scp -o ControlMaster=auto -o ControlPath=/tmp/sw-%r@%h:%p -o ControlPersist=60s

.PHONY: install build test ci contract purity-check package publish publish-r2 \
        publish-winget publish-scoop publish-packages \
        pull-releases require-win-remote-host sync-win-host win-host-ci help

help:
	@echo "verbs: install build test ci contract purity-check package publish smoke run clean"
	@echo "release: package (box) -> publish (box) -> pull-releases -> publish-r2 -> publish-packages"
	@echo "ci = local fast checks + the remote Windows build/test; needs WIN_REMOTE_HOST=user@host"

# Local dev-tooling setup. The Rust/MSVC toolchain is remote (see win-host-ci);
# locally we only set up the UI's JS deps when present. Run by the hopper mill at
# lode start.
install:
	@if [ -f ui/package.json ]; then npm --prefix ui install; else echo "no local tooling to install"; fi

# Build the webview bundle + the binary. The webview is built FIRST: Tauri embeds
# ui/dist into the exe at cargo-compile time, so building it after would embed a
# stale bundle.
build:
	npm --prefix ui run build
	$(CARGO) build -p $(TAURI_BIN) --features custom-protocol

# Local cross-platform tests (pure tier + capture-engine), host-testable, no live
# target. The windows-only crates test remotely via win-host-ci.
test:
	$(CARGO) test --workspace $(REMOTE_CRATES)

# The one CI surface for the engineer: cheap, host-independent checks run locally
# and fail fast, then the real Windows build + test runs on the build box. One
# flow. fmt/deny/contract/purity are host-independent; clippy + test cover the
# cross-platform crates (pure tier + capture-engine). The windows-only crates are
# built and tested remotely by win-host-ci.
ci:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace $(REMOTE_CRATES) --all-targets -- -D warnings
	$(CARGO) run -q -p xtask -- contract --check
	$(CARGO) run -q -p xtask -- purity-check
	$(CARGO) test --workspace $(REMOTE_CRATES)
	$(CARGO) deny check
	$(MAKE) win-host-ci

# Regenerate automation-contract.json + the ui codegen; the operator commits.
contract:
	$(CARGO) run -q -p xtask -- contract

# Structural gate: the `windows` family must never reach the pure tier
# (AGENTS.md §Source Layout). `--target all` makes target-gated leaks visible on any host.
purity-check:
	$(CARGO) run -q -p xtask -- purity-check

# Build a RELEASE binary + webview, then pack a Velopack release into Releases/.
# Release (not the debug `build`) so the tray app is windowless — the
# `windows_subsystem="windows"` attribute is release-only. The webview is built
# first (embedded at cargo-compile time). The .ps1 consumes target/release/ and
# does not rebuild. Unsigned now; the $SignTemplate seam in scripts/package.ps1 is
# empty until the cert lands.
package:
	npm --prefix ui run build
	# --features custom-protocol: serve the embedded ui/dist, not the Vite devUrl.
	# Without it the shipped Settings/About load "localhost refused to connect"
	# (cargo tauri build sets it automatically; a plain cargo build does not).
	$(CARGO) build -p $(TAURI_BIN) --release --features custom-protocol
	$(PWSH) -File scripts/package.ps1

# Upload the Releases/ dir to GitHub Releases = the demoted source-hygiene mirror
# (a tagged source release with the artifacts attached). The PRIMARY auto-update
# feed is R2 (publish-r2). Runs on the build box.
publish:
	$(PWSH) -File scripts/publish.ps1

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
sync-win-host: require-win-remote-host
	@mkdir -p $(dir $(WIN_BUNDLE))
	@SHA=$$(git stash create); [ -n "$$SHA" ] || SHA=$$(git rev-parse HEAD); \
	  git update-ref refs/heads/__swsync $$SHA; \
	  git bundle create $(WIN_BUNDLE) refs/heads/__swsync; \
	  git update-ref -d refs/heads/__swsync; \
	  echo "synced working tree @ $$SHA"
	$(WIN_SCP) $(WIN_BUNDLE) $(WIN_REMOTE_HOST):swbuild.bundle

# Sync, then run the Session-0-safe gate on the box (build + tests + contract).
# The live FlaUI smoke + lifecycle matrix are operator-direct, not part of this.
win-host-ci: sync-win-host
	$(WIN_SSH) $(WIN_REMOTE_HOST) 'cmd /c C:\sol\sw-ci.cmd'
