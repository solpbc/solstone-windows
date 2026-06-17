// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
//
// GENERATED — DO NOT EDIT. Run `make contract`.
// Source of truth: the observer-contract crate -> automation-contract.json.

export const automationContract = {
  "_generated": "DO NOT EDIT — run make contract",
  "automation_ids": {
    "about.version": "about.version",
    "about.window.root": "about.window.root",
    "settings.sources.mic.state": "settings.sources.mic.state",
    "settings.sources.screen.state": "settings.sources.screen.state",
    "settings.sources.systemAudio.state": "settings.sources.systemAudio.state",
    "settings.status.appState.state": "settings.status.appState.state",
    "settings.status.segmentDir": "settings.status.segmentDir",
    "settings.window.root": "settings.window.root",
    "tray.menu.about": "tray.menu.about",
    "tray.menu.openSettings": "tray.menu.openSettings",
    "tray.menu.pause": "tray.menu.pause",
    "tray.menu.quit": "tray.menu.quit",
    "tray.menu.resume": "tray.menu.resume",
    "tray.menu.start": "tray.menu.start",
    "tray.root": "tray.root"
  },
  "state_tokens": {
    "app_phase": [
      "error",
      "idle",
      "observing",
      "paused",
      "starting"
    ],
    "error_reason": [
      "access_denied",
      "endpoint_lost",
      "unknown",
      "write_failed"
    ],
    "pause_reason": [
      "operator",
      "session_locked",
      "system_suspending"
    ],
    "source_kind": [
      "mic",
      "screen",
      "system_audio"
    ],
    "source_status": [
      "active",
      "faulted",
      "inactive",
      "no_input_device"
    ]
  }
} as const;

export type AutomationContract = typeof automationContract;
