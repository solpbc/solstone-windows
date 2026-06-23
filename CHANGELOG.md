# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-06-23

### Added

- solstone now runs on Windows. it lives in your tray, pairs to your journal from a
  pairing link, and observes your screen and audio alongside you, sending what it
  observes to your journal over a private, signed connection.
- the installer is signed by sol pbc, so Windows can verify it came from us. it
  installs just for you, with no admin prompt.
- sensitive apps and private-browsing windows are left out of what's observed by
  default. you decide what's included from Settings.
- you're in control of observing from the tray and Settings: pause for a set time or
  until you resume (with a global hotkey), stop entirely, pick which microphone is
  used or turn the mic off, and set how long already-delivered data stays cached on
  your machine before it's cleared.
- solstone keeps itself up to date over a signed, private channel. the update check
  carries no identifier about you or your machine and reaches only solstone's own
  update service. an Updates section in Settings lets you check now, set how often to
  check, download in the background, and see when it last checked.
