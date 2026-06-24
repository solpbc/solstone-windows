# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3] - 2026-06-24

### Changed

- Settings and About now feel native to Windows 11: they use the Windows system
  font, follow your light or dark mode, sit over a soft translucent backdrop, and
  use native scrollbars.
- these windows no longer act like web pages: right-click menus, rubber-band
  scroll, pinch zoom, and keyboard zoom stay out of your way.

## [0.2.2] - 2026-06-24

### Added

- the tray icon now shows whether your data is reaching your journal: a full sun
  when you're observing and connected, a half sun when you're observing but not
  connected to a journal yet (so "observing" never overstates what's happening).

### Changed

- the app is now named "solstone" (lowercase) everywhere you see it — taskbar,
  Start menu, installer — to match the brand.
- a new rounded app icon, built for Windows (rounded tile, sharp at every size).
- crisper tray icon at small sizes.
- opening solstone now brings up the Settings window so you can see it's running,
  instead of quietly going to the tray. (launching at login still stays quiet in
  the tray.)

## [0.2.1] - 2026-06-24

### Fixed

- Settings and About now open correctly. in 0.2.0 these windows could come up
  blank; they now show as intended, so you can pair to your journal and manage
  observing from Settings.

### Changed

- the update check is now a bare request for the version manifest — it carries
  nothing about the version you have or your install. it was already private; this
  trims the last detail off the wire.

### Added

- a local diagnostic log under your Solstone app data, to make support easier. it
  stays on your machine — nothing is ever sent anywhere — and it records only
  events and errors, never the contents of what's observed.

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
