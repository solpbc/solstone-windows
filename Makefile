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
	        test-scripts gate-minisign ci audit contract purity-check check-observer-contract check-rust-release-manifest check-release-advisory-config package prove-rust-release-native publish-transparency resign-transparency-pointer publish publish-r2 \
	        publish-winget publish-scoop publish-packages check-channels \
	        pull-releases require-win-remote-host sync-win-host win-host-ci \
	        smoke screenshots journal-live help

help:
	@echo "verbs: install ui-deps-update rust-toolchain provision-cargo-deny build test ci audit contract purity-check check-observer-contract check-rust-release-manifest check-release-advisory-config package prove-rust-release-native publish-transparency resign-transparency-pointer smoke screenshots journal-live run clean"
	@echo "release: package runs the source-bound provenance transaction -> target/release-candidate/<VERSION>/ (requires EXPECTED_RELEASE_COMMIT, SOLSTONE_ADVISORY_TREE_SHA256, and the signed mirror packet environment)"
	@echo "proof: prove-rust-release-native RELEASE_DIR=<candidate> installs and smokes one exact signed candidate"
	@echo "ci = local fast checks + the remote Windows build/test; needs WIN_REMOTE_HOST=user@host"

# Local dev-tooling setup. The Rust/MSVC toolchain is remote (see win-host-ci);
# locally we only set up the UI's JS deps when present. Run during local workspace setup.
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

# Host-testable deterministic/package/publication policy checks on the Linux host.
test-scripts:
	sh scripts/lib/deterministic-gates.test.sh
	sh scripts/lib/publication-guard.test.sh
	sh scripts/lib/transparency-guard.test.sh
	sh scripts/lib/make-package-ordering.test.sh
	sh scripts/lib/make-prove-native-ordering.test.sh
	sh scripts/lib/doc-stale-scan.test.sh

# UI unit tests (vitest+jsdom) on the Linux host. Materialize only the committed
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
	MANIFEST= RELEASE_DIR= $(MAKE) check-rust-release-manifest
	$(CARGO) test --locked --workspace $(REMOTE_CRATES)
	$(CARGO) deny --offline --locked check bans licenses sources
	$(MAKE) check-release-advisory-config
	$(MAKE) ui-test
	$(MAKE) test-scripts
	$(MAKE) gate-minisign
	$(MAKE) win-host-ci

# Refresh the RustSec advisory database, then check it against the locked graph.
# This networked freshness check is deliberately separate from deterministic CI.
audit: preflight-toolchain preflight-cargo-deny
	@$(CARGO) deny fetch db || { echo "ERROR: RustSec advisory database refresh failed; no current advisory result was produced." >&2; exit 1; }
	$(CARGO) deny --locked check advisories

# Regenerate automation-contract.json + the ui codegen; the operator commits.
contract: preflight-toolchain
	$(CARGO) run --locked -q -p xtask -- contract

# Structural gate: the `windows` family must not reach a strict member's shipped
# normal+build graph. Dev-only reachability never ships; `xtask` is reviewed tooling.
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

# Offline schema, semantic, ledger, current-bundle, and deterministic-render gate.
check-rust-release-manifest: preflight-toolchain
	@echo "offline Rust release-manifest evidence"
	MANIFEST="$(MANIFEST)" RELEASE_DIR="$(RELEASE_DIR)" CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- rust-release-manifest check
	CARGO_NET_OFFLINE=true $(CARGO) test --locked -p xtask rust_release_manifest

# Offline real-pin acceptance for the deterministic release advisory config.
# The default cargo-deny cache is only the source snapshot; the check itself
# always uses the isolated target/release-advisory-db path written into config.
check-release-advisory-config: preflight-toolchain preflight-cargo-deny
	@set -eu; \
	  mirror_locator="$${SOLSTONE_ADVISORY_MIRROR_LOCATOR:-}"; \
	  if [ -z "$$mirror_locator" ]; then \
	    echo "ERROR: SOLSTONE_ADVISORY_MIRROR_LOCATOR is required; set it to the approved private mirror Git URL and retry." >&2; \
	    exit 1; \
	  fi; \
	  cargo_home="$${CARGO_HOME:-$$HOME/.cargo}"; \
	  host_db_root="$$cargo_home/advisory-dbs"; \
	  host_repo=; \
	  for candidate in "$$host_db_root"/advisory-db-*; do \
	    [ -d "$$candidate/.git" ] || continue; \
	    if [ -n "$$host_repo" ]; then \
	      echo "ERROR: multiple advisory repositories found under $$host_db_root; use a clean/isolated cargo home containing the approved mirror cache, then retry." >&2; \
	      exit 1; \
	    fi; \
	    host_repo=$$candidate; \
	  done; \
	  if [ -z "$$host_repo" ]; then \
	    echo "ERROR: advisory mirror database is absent under $$host_db_root; use a clean/isolated cargo home containing the approved mirror cache, then retry." >&2; \
	    exit 1; \
	  fi; \
	  repo_root=$$(pwd -P); \
	  if [ -L "$$repo_root/target" ]; then \
	    echo "ERROR: target is a symlink; restore a real checkout-local target directory, then retry." >&2; \
	    exit 1; \
	  fi; \
	  mkdir -p "$$repo_root/target"; \
	  isolated="$$repo_root/target/release-advisory-db"; \
	  if [ -L "$$isolated" ] || { [ -e "$$isolated" ] && [ ! -d "$$isolated" ]; }; then \
	    echo "ERROR: target/release-advisory-db is not a real directory; remove the unsafe entry and retry." >&2; \
	    exit 1; \
	  fi; \
	  stage=$$(mktemp -d "$$repo_root/target/.release-advisory-db-map.XXXXXX"); \
	  db_lock=; \
	  cleanup() { \
	    if [ -n "$$db_lock" ] && [ -f "$$db_lock" ] && [ ! -L "$$db_lock" ]; then rm -f "$$db_lock"; fi; \
	    rm -rf "$$stage"; \
	  }; \
	  trap cleanup EXIT HUP INT TERM; \
	  mkdir "$$stage/new"; \
	  cp -a "$$host_repo" "$$stage/new/"; \
	  repo_name=$${host_repo##*/}; \
	  [ -d "$$stage/new/$$repo_name/.git" ] || { echo "ERROR: mapped advisory cache lacks its Git repository; use a clean/isolated cargo home containing the approved mirror cache, then retry." >&2; exit 1; }; \
	  previous=; \
	  if [ -d "$$isolated" ]; then \
	    previous="$$stage/previous"; \
	    mv "$$isolated" "$$previous"; \
	  fi; \
	  if ! mv "$$stage/new" "$$isolated"; then \
	    [ -z "$$previous" ] || mv "$$previous" "$$isolated"; \
	    echo "ERROR: isolated RustSec cache mapping failed; restore target permissions and retry." >&2; \
	    exit 1; \
	  fi; \
	  [ -z "$$previous" ] || rm -rf "$$previous"; \
	  check_dir="$$repo_root/target/release-advisory-config-check"; \
	  if [ -L "$$check_dir" ] || { [ -e "$$check_dir" ] && [ ! -d "$$check_dir" ]; }; then \
	    echo "ERROR: advisory config check output is not a real directory; remove the unsafe entry and retry." >&2; \
	    exit 1; \
	  fi; \
	  mkdir -p "$$check_dir"; \
	  config="$$check_dir/deny.toml"; \
	  if [ -L "$$config" ] || { [ -e "$$config" ] && [ ! -f "$$config" ]; }; then \
	    echo "ERROR: advisory config check output is not a regular file; remove the unsafe entry and retry." >&2; \
	    exit 1; \
	  fi; \
	  rm -f "$$config"; \
	  SOLSTONE_ADVISORY_MIRROR_LOCATOR="$$mirror_locator" CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- rust-release-manifest advisory-config --db-root "$$isolated" --out "$$config"; \
	  $(CARGO) deny --locked --version; \
	  db_lock="$$isolated/db.lock"; \
	  CARGO_NET_OFFLINE=true $(CARGO) deny --locked --offline --config "$$config" check advisories; \
	  if [ -L "$$db_lock" ] || { [ -e "$$db_lock" ] && [ ! -f "$$db_lock" ]; }; then \
	    echo "ERROR: cargo-deny advisory lock is not a regular file; remove the unsafe target/release-advisory-db/db.lock entry and retry." >&2; \
	    exit 1; \
	  fi; \
	  rm -f "$$db_lock"

# Thin bootstrap into the one source-bound build-to-finalize transaction.
# package.ps1 performs defense-in-depth preflight/version/lock gates; xtask owns
# the authoritative source binding and every native build/package action.
package:
	@set -eu; \
	  if [ -z "$${EXPECTED_RELEASE_COMMIT:-}" ]; then \
	    echo "ERROR: EXPECTED_RELEASE_COMMIT is required; set it to the full lowercase 40-hex release commit and retry." >&2; \
	    exit 1; \
	  fi; \
	  advisory_digest="$${SOLSTONE_ADVISORY_TREE_SHA256:-}"; \
	  if [ "$${#advisory_digest}" -ne 64 ]; then \
	    echo "ERROR: SOLSTONE_ADVISORY_TREE_SHA256 is required as 64 lowercase hex; supply the reviewed isolated RustSec archive digest and retry." >&2; \
	    exit 1; \
	  fi; \
	  case "$$advisory_digest" in *[!0-9a-f]*) echo "ERROR: SOLSTONE_ADVISORY_TREE_SHA256 is required as 64 lowercase hex; supply the reviewed isolated RustSec archive digest and retry." >&2; exit 1 ;; esac; \
	  if [ -z "$${SOLSTONE_ADVISORY_MIRROR_LOCATOR:-}" ]; then \
	    echo "ERROR: SOLSTONE_ADVISORY_MIRROR_LOCATOR is required; set the approved private mirror locator and retry." >&2; \
	    exit 1; \
	  fi; \
	  if [ -z "$${SOLSTONE_ADVISORY_RECEIPT:-}" ]; then \
	    echo "ERROR: SOLSTONE_ADVISORY_RECEIPT is required; set the signed mirror freshness receipt body path and retry." >&2; \
	    exit 1; \
	  fi; \
	  if [ -z "$${SOLSTONE_ADVISORY_MIRROR_PUB:-}" ]; then \
	    echo "ERROR: SOLSTONE_ADVISORY_MIRROR_PUB is required; set the approved mirror public-key path and retry." >&2; \
	    exit 1; \
	  fi; \
	  sign_arg=; \
	  case "$${SOLSTONE_SIGN:-}" in \
	    "") ;; \
	    1) sign_arg=-Sign ;; \
	    *) echo "ERROR: SOLSTONE_SIGN must be exactly 1 when signing is requested; unset it for unsigned finalization and retry." >&2; exit 1 ;; \
	  esac; \
	  if [ -n "$$sign_arg" ]; then \
	    GIT="$(GIT)" $(PWSH) -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1 -Sign; \
	  else \
	    GIT="$(GIT)" $(PWSH) -NoProfile -ExecutionPolicy Bypass -File scripts/package.ps1; \
	  fi

# Strict native install/smoke proof for one already-finalized signed candidate.
# Build then invoke directly: cargo run adds rustup's toolchain bin to PATH, making
# signed-preflight cargo/rustc resolution ambiguous. configured_cargo() otherwise
# loses cargo run's injected CARGO; SOLSTONE_VERSION_GATE_CARGO prevents PATH fallback.
prove-rust-release-native:
	@set -eu; \
	  if [ -z "$(RELEASE_DIR)" ]; then \
	    echo "ERROR: RELEASE_DIR is required; pass target/release-candidate/<VERSION> and retry." >&2; \
	    exit 1; \
	  fi; \
	  target_dir="$${CARGO_TARGET_DIR:-target}"; \
	  CARGO_NET_OFFLINE=true $(CARGO) build --locked -q -p xtask; \
	  xtask_bin="$$target_dir/debug/xtask"; \
	  if [ ! -x "$$xtask_bin" ]; then \
	    xtask_bin="$$target_dir/debug/xtask.exe"; \
	  fi; \
	  if [ ! -x "$$xtask_bin" ]; then \
	    echo "ERROR: built xtask executable not found at $$target_dir/debug/xtask or $$target_dir/debug/xtask.exe; restore CARGO_TARGET_DIR consistency and retry." >&2; \
	    exit 1; \
	  fi; \
	  SOLSTONE_PROOF_POWERSHELL="$(PWSH)" SOLSTONE_VERSION_GATE_CARGO="$(CARGO)" CARGO_NET_OFFLINE=true "$$xtask_bin" rust-release-manifest prove-native --release-dir "$(RELEASE_DIR)"

# Publish evidence for one already-delivered validated candidate. Artifact bytes
# go only to the operator archive channel; the public surface receives evidence.
publish-transparency:
	@set -eu; \
	  if [ -z "$(RELEASE_DIR)" ]; then \
	    echo "ERROR: RELEASE_DIR is required; pass target/release-candidate/<VERSION> and retry transparency publication." >&2; \
	    exit 1; \
	  fi; \
	  CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- transparency publish --release-dir "$(RELEASE_DIR)"

# Refresh only the signed latest pointer. This deliberately has no candidate input.
resign-transparency-pointer:
	CARGO_NET_OFFLINE=true $(CARGO) run --locked -q -p xtask -- transparency resign-pointer

# Real-tool acceptance for the declared minisign development prerequisite.
gate-minisign:
	sh scripts/gate-minisign.sh

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
