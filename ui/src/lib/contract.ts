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
    "settings.exclusions.activity": "settings.exclusions.activity",
    "settings.exclusions.appAdd": "settings.exclusions.appAdd",
    "settings.exclusions.appInput": "settings.exclusions.appInput",
    "settings.exclusions.appsList": "settings.exclusions.appsList",
    "settings.exclusions.privateBrowsing": "settings.exclusions.privateBrowsing",
    "settings.exclusions.titleAdd": "settings.exclusions.titleAdd",
    "settings.exclusions.titleInput": "settings.exclusions.titleInput",
    "settings.exclusions.titlesList": "settings.exclusions.titlesList",
    "settings.home.kinship": "settings.home.kinship",
    "settings.hotkey.clear": "settings.hotkey.clear",
    "settings.hotkey.combo": "settings.hotkey.combo",
    "settings.hotkey.enabled": "settings.hotkey.enabled",
    "settings.hotkey.status": "settings.hotkey.status",
    "settings.journal.open": "settings.journal.open",
    "settings.journal.unavailable": "settings.journal.unavailable",
    "settings.mic.active": "settings.mic.active",
    "settings.mic.devices": "settings.mic.devices",
    "settings.mic.gain": "settings.mic.gain",
    "settings.pairing.input": "settings.pairing.input",
    "settings.pairing.journal": "settings.pairing.journal",
    "settings.pairing.state": "settings.pairing.state",
    "settings.pairing.submit": "settings.pairing.submit",
    "settings.retention": "settings.retention",
    "settings.sources.mic.state": "settings.sources.mic.state",
    "settings.sources.screen.state": "settings.sources.screen.state",
    "settings.sources.systemAudio.state": "settings.sources.systemAudio.state",
    "settings.status.appState.state": "settings.status.appState.state",
    "settings.status.segmentDir": "settings.status.segmentDir",
    "settings.status.upload.state": "settings.status.upload.state",
    "settings.updates.autoCheck": "settings.updates.autoCheck",
    "settings.updates.autoDownload": "settings.updates.autoDownload",
    "settings.updates.cancel": "settings.updates.cancel",
    "settings.updates.checkNow": "settings.updates.checkNow",
    "settings.updates.dismiss": "settings.updates.dismiss",
    "settings.updates.download": "settings.updates.download",
    "settings.updates.frequency": "settings.updates.frequency",
    "settings.updates.install": "settings.updates.install",
    "settings.updates.lastChecked": "settings.updates.lastChecked",
    "settings.updates.notes": "settings.updates.notes",
    "settings.updates.retry": "settings.updates.retry",
    "settings.updates.state": "settings.updates.state",
    "settings.window.root": "settings.window.root",
    "tray.menu.about": "tray.menu.about",
    "tray.menu.openJournal": "tray.menu.openJournal",
    "tray.menu.openSettings": "tray.menu.openSettings",
    "tray.menu.pause": "tray.menu.pause",
    "tray.menu.pause15m": "tray.menu.pause15m",
    "tray.menu.pause1h": "tray.menu.pause1h",
    "tray.menu.pause30m": "tray.menu.pause30m",
    "tray.menu.pauseIndefinite": "tray.menu.pauseIndefinite",
    "tray.menu.quit": "tray.menu.quit",
    "tray.menu.restartObserving": "tray.menu.restartObserving",
    "tray.menu.resume": "tray.menu.resume",
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
    "pairing_phase": [
      "failed",
      "not_paired",
      "paired",
      "pairing"
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
    ],
    "view_render_state": [
      "pending",
      "rendered"
    ]
  }
} as const;

export type AutomationContract = typeof automationContract;
