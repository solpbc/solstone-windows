// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);
const T0 = Date.UTC(2026, 9, 20, 12, 0, 0);

const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function resetRoot(): void {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  document.body.append(rootEl);
  app.__test__.reset();
  app.__test__.setRoot(rootEl);
}

function at(secs: number): void {
  vi.setSystemTime(T0 + secs * 1000);
}

function digits(id: string): string {
  return (byId(id)?.textContent ?? "").replace(/[^0-9:]/g, "");
}

describe("home status clocks", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    at(0);
    resetRoot();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined as never);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("counts the next save down between readings and wraps at the boundary", () => {
    const dump = { ...observingDump(), segment_seconds_remaining: 125 };
    app.__test__.setRoute("home");
    app.__test__.renderSettings(dump as never);
    expect(digits(ids["settings.status.nextSave"])).toBe("2:05");

    at(5);
    app.__test__.updateStatusClocks();
    expect(digits(ids["settings.status.nextSave"])).toBe("2:00");

    at(126);
    app.__test__.updateStatusClocks();
    expect(digits(ids["settings.status.nextSave"])).toBe("4:59");
  });

  it("shows no countdown unless taking things in", () => {
    const dump = { ...observingDump(), app_state: "paused", segment_seconds_remaining: 125 };
    app.__test__.setRoute("home");
    app.__test__.renderSettings(dump as never);
    expect(byId(ids["settings.status.nextSave"])).toBeNull();
  });

  it("ages the last sync while the window stays open", () => {
    const dump = observingDump();
    (dump.sync.upload as Record<string, unknown>).last_successful_sync = T0 - 30_000;
    app.__test__.setRoute("home");
    app.__test__.renderSettings(dump as never);
    const before = byId(ids["settings.status.lastSynced"])?.textContent;

    at(3 * 60);
    app.__test__.updateStatusClocks();
    const after = byId(ids["settings.status.lastSynced"])?.textContent;
    expect(digits(ids["settings.status.lastSynced"])).toBe("3");
    expect(after).not.toBe(before);
  });

  it("offers the Windows privacy page when the microphone is refused", () => {
    const dump = {
      ...observingDump(),
      setup: {
        sequence_enabled: true,
        mic: true,
        system_audio: true,
        screen: true,
        start_at_login: false,
        wizard: "done" as const,
        notice_dismissed: true,
      },
      sources: [
        { kind: "screen", status: "active", device: "Display 1" },
        { kind: "mic", status: "faulted", reason: "access_denied", detail: "", device: null },
      ],
    };
    app.__test__.setRoute("sources");
    app.__test__.renderSettings(dump as never);

    const button = byId(ids["settings.sources.mic.permission"]) as HTMLButtonElement | null;
    expect(button).not.toBeNull();
    button!.click();
    expect(invokeMock).toHaveBeenCalledWith("open_microphone_privacy_settings");
  });
});
