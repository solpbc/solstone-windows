// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

export type UpdateDisplayKind =
  | "never_checked"
  | "up_to_date"
  | "checking"
  | "available"
  | "downloading"
  | "staged"
  | "failed"
  | "failed_with_available"
  | "unavailable";

type PartialDeep<T> = {
  [K in keyof T]?: T[K] extends object ? PartialDeep<T[K]> : T[K];
};

interface UpdateViewFixture {
  display: UpdateDisplayKind;
  activity: "idle" | "checking" | "downloading" | "installing";
  last_checked_at: number | null;
  available_version: string | null;
  notes: string | null;
  download_pct: number | null;
  prefs: {
    auto_check: boolean;
    interval: "day" | "week" | "month";
    auto_download: boolean;
  };
  actions: {
    can_check_now: boolean;
    can_cancel: boolean;
    can_download: boolean;
    can_install: boolean;
    can_retry: boolean;
    can_dismiss: boolean;
    frequency_enabled: boolean;
  };
}

function uploadStatus() {
  return {
    pending_segments: 0,
    uploaded_segments: 1,
    failed_segments: 0,
    quarantined_segments: 0,
    last_uploaded_segment: null,
    last_error: null,
  };
}

export function observingDump() {
  return {
    app_state: "observing",
    sources: [
      { kind: "screen", status: "active", device: "Display 1" },
      { kind: "system_audio", status: "active", device: "Speakers" },
      { kind: "mic", status: "active", device: "USB Mic" },
    ],
    frame_rate: null,
    segment_dir: null,
    segment_seconds_remaining: null,
    engine_ready: true,
    version: "test-version",
    sync: {
      pairing: {
        phase: "paired",
        journal_label: "journal",
        detail: null,
      },
      upload: uploadStatus(),
    },
    screen_encoder: null,
    exclusions: null,
    pause: null,
    views: {},
  };
}

export function pausedDump(secondsRemaining: number) {
  return {
    ...observingDump(),
    app_state: "paused",
    pause: {
      reason: "operator",
      seconds_remaining: secondsRemaining,
    },
  };
}

export function notPairedDump() {
  return {
    ...observingDump(),
    sync: {
      pairing: {
        phase: "not_paired",
        journal_label: null,
        detail: null,
      },
      upload: {
        ...uploadStatus(),
        uploaded_segments: 0,
      },
    },
  };
}

export function faultedSourceDump() {
  return {
    ...observingDump(),
    app_state: "error",
    sources: [
      {
        kind: "screen",
        status: "faulted",
        reason: "access_denied",
        detail: "screen denied",
        device: "Display 1",
      },
      { kind: "system_audio", status: "active", device: "Speakers" },
      { kind: "mic", status: "active", device: "USB Mic" },
    ],
  };
}

export function exclusionsDump() {
  return {
    ...observingDump(),
    exclusions: {
      rules_active: true,
      frames_redacted: 3,
      frames_dropped: 1,
    },
  };
}

export function exclusionRules(values?: { exes?: string[]; titles?: string[] }) {
  return {
    excluded_exes: values?.exes ?? ["secret.exe", "notes.exe"],
    title_patterns: values?.titles ?? ["banking", "medical"],
    exclude_private_browsing: true,
  };
}

export function micDeviceList() {
  return [
    { id: "array", name: "Array Mic" },
    { id: "usb", name: "USB Mic" },
    { id: "webcam", name: "Webcam Mic" },
  ];
}

export function micView(overrides?: PartialDeep<ReturnType<typeof baseMicView>>) {
  const base = baseMicView();
  return {
    ...base,
    ...overrides,
    config: {
      ...base.config,
      ...overrides?.config,
    },
  };
}

function baseMicView() {
  return {
    config: {
      priority: ["usb", "array"],
      disabled: ["array"],
      gain: 4,
    },
    active_id: "usb",
  };
}

export function updateView(
  display: UpdateDisplayKind,
  overrides: PartialDeep<UpdateViewFixture> = {},
): UpdateViewFixture {
  const base: UpdateViewFixture = {
    display,
    activity: "idle",
    last_checked_at: 1_700_000_000,
    available_version: null,
    notes: null,
    download_pct: null,
    prefs: {
      auto_check: true,
      interval: "day",
      auto_download: false,
    },
    actions: {
      can_check_now: true,
      can_cancel: false,
      can_download: false,
      can_install: false,
      can_retry: false,
      can_dismiss: false,
      frequency_enabled: true,
    },
  };

  if (display === "never_checked") {
    base.last_checked_at = null;
  } else if (display === "available") {
    base.available_version = "0.2.1";
    base.notes = "## 0.2.1\n- One fix";
    base.actions.can_download = true;
  } else if (display === "downloading") {
    base.activity = "downloading";
    base.available_version = "0.2.1";
    base.download_pct = 42;
    base.actions.can_check_now = false;
  } else if (display === "staged") {
    base.available_version = "0.2.1";
    base.actions.can_install = true;
  } else if (display === "failed") {
    base.actions.can_retry = true;
  }

  return {
    ...base,
    ...overrides,
    prefs: {
      ...base.prefs,
      ...overrides.prefs,
    },
    actions: {
      ...base.actions,
      ...overrides.actions,
    },
  };
}

export function sampleMarkSpec(word1 = "piano", word2 = "key", colorHex = "#3b82f6") {
  return {
    icon1: {
      name: "piano",
      svg: '<path d="M18.5 8c-1.4 0-2.6-.8-3.2-2A6.87 6.87 0 0 0 2 9v11a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-8.5C22 9.6 20.4 8 18.5 8" /><path d="M2 14h20" />',
      color: {
        name: "blue",
        hex: colorHex,
      },
      rot: 45,
    },
    icon2: {
      name: "key",
      svg: '<path d="m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4" /><path d="m21 2-9.6 9.6" /><circle cx="7.5" cy="15.5" r="5.5" />',
      color: {
        name: "purple",
        hex: "#a855f7",
      },
      rot: 0,
    },
    words: [word1, word2] as [string, string],
  };
}

export function pairedWithMarkDump(mark?: ReturnType<typeof sampleMarkSpec> | null) {
  const base = observingDump();
  return {
    ...base,
    sync: {
      ...base.sync,
      pairing: {
        ...base.sync.pairing,
        mark: mark !== undefined ? mark : sampleMarkSpec("liquefy", "smock", "#3b82f6"),
      },
    },
  };
}

export function unknownJournalsDump(sightings: Array<{
  address?: string | null;
  expected_mark?: ReturnType<typeof sampleMarkSpec>;
  responding_mark?: ReturnType<typeof sampleMarkSpec> | null;
}>) {
  const base = observingDump();
  return {
    ...base,
    sync: {
      ...base.sync,
      unknown_journals: sightings.map((s) => ({
        address: s.address !== undefined ? s.address : "192.168.1.50:443",
        expected_mark: s.expected_mark ?? sampleMarkSpec("liquefy", "smock", "#3b82f6"),
        responding_mark: s.responding_mark !== undefined ? s.responding_mark : sampleMarkSpec("distrust", "chokehold", "#ec4899"),
      })),
    },
  };
}
