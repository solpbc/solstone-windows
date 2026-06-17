# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial public bootstrap: the cargo workspace, the three crate tiers
  (pure / platform / composition), the Tauri v2 tray-app skeleton, the Vite
  webview skeleton, the net48 FlaUI harness skeleton, Velopack packaging scaffold,
  the `make` verb surface, and the generated, drift-gated `automation-contract.json`.
- The pure tier is host-testable: rotation math, the honest-state reducer,
  incomplete-segment recovery, the backoff/circuit-breaker, and the contract
  generator all run off-Windows.
