# Journal Window Live Validation

This is an operator-run validation for the native Journal window on the Windows
build box. It is not part of `make ci` or `win-host-ci` because it requires an
interactive Session 1 desktop.

Run:

```text
make journal-live
```

Expected success marker:

```text
JOURNAL_WINDOW_LIVE_OK
```

The driver writes artifacts under:

```text
target/journal-window-live/<timestamp>/
```

Evidence files:

- `pairing.backup` or `pairing.absent`: exact prior pairing state.
- `mock.stdout.log` / `mock.stderr.log`: mock journal process logs.
- `mock-ready.json`: mock port and marker.
- `mock-transcript.ndjson`: one request per line, including carrier index,
  stream id, method, path, and observer-auth header presence.
- `window-evidence.json`: title, rect, visibility, minimized, cloaked, and
  screenshot path.
- `journal.png`: Session 1 screenshot of the selected Journal window.
- `result.txt`: final ok/fail marker.

The harness stages a mock `pairing.json`, launches the installed app in Session
1, invokes the native tray `Open Journal` item by AutomationId
`tray.menu.openJournal`, and then asserts both:

- Session 1 saw a normal visible app window, at least 640x480, not minimized or
  cloaked.
- The mock journal saw `GET /`, `GET /asset-a`, and `GET /asset-b` on one PL
  carrier, with observer auth injected by the app bridge.

The harness never fetches `/`, `/asset-a`, or `/asset-b` itself. Those requests
must come from the app-opened WebView through the loopback bridge.

Tray-trigger caveat: on some Windows builds the tray context menu may not be
reachable on the UIA surface without interactive opening. If that happens, the
documented alternate real-user route is to launch Settings and invoke the
`settings.journal.open` button; keep the same mock transcript and window
assertions as the source of truth.
