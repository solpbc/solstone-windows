# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- the observer now starts on a clean windows machine that has never had the visual c++ runtime installed.

## [0.2.6] - 2026-07-01

### Added

- open your journal right from solstone: a new "Open Journal" entry point in the
  tray and in settings opens your paired journal in its own window, already
  signed in over your existing connection. no browser, no separate sign-in.

### Changed

- pairing your journal over the private network now uses a one-time pairing link
  your journal opens for you. there is no shared code to copy or time. pair once
  and solstone connects to your journal from anywhere: over the encrypted relay
  when you are away, and directly when you are home.
- app icon refreshed to the current sol mark.

### Fixed

- the pending count on the home pane no longer freezes partway through an
  upload; it now counts down as each item finishes.
- settings no longer flickers every second; it now only redraws when something
  on the screen actually changes.

## [0.2.5] - 2026-06-28

### Added

- reach your journal from anywhere. when your journal has private network access
  turned on, solstone now connects to it over the encrypted relay while you're
  away from home, not only on the same wi-fi. it keeps using a direct connection
  when you're home and switches to the relay when you're not, with nothing to set
  up beyond pairing. the relay only carries sealed traffic between your device and
  your journal; it never sees what's inside.

## [0.2.4] - 2026-06-25

### Added

- a home section in settings: a status overview (what solstone is doing, your
  journal, your sources) plus quick actions in one place: pause or resume, pair
  your journal, open your local folder, and check for updates.

### Changed

- settings now has a section menu down the left side, like Windows Settings,
  instead of one long scroll. pick a section to jump straight to it, and the
  window reflows as you resize it, collapsing the menu to a button when narrow.
- settings feels more at home on Windows 11: native scrollbars, keyboard focus
  rings, your accent color, and rounded controls.
- the status view is clearer and more honest: it shows "stored on this pc" with
  an open-folder button instead of an internal path, tells you when you are not
  yet paired instead of a confusing "0 delivered", shows each source's state as a
  simple label, and lists apps by their friendly names when you exclude them.

### Fixed

- drop-down menus stay open long enough to pick an option. they could close
  before your click landed.
- you can now select and copy your local storage path.

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
