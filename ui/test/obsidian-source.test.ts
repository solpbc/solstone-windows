// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);

type Status = Parameters<typeof app.__test__.setObsidian>[0];

const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function status(overrides: Partial<NonNullable<Status>> = {}): NonNullable<Status> {
  return {
    state: "on",
    vault: "Notes",
    warning: null,
    last_checked_ms: null,
    waiting: 0,
    first_sync: null,
    not_on_device: 0,
    too_large: 0,
    attention: null,
    ...overrides,
  };
}

function render(
  value: Status,
  found: { name: string; path: string }[] = [],
  refusal: "not_a_vault" | "unusable_name" | null = null,
): void {
  const dump = observingDump();
  app.__test__.setRoute("sources");
  app.__test__.setHealth(dump);
  app.__test__.setObsidian(value, found, refusal);
  app.__test__.renderSettings(dump);
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

describe("Obsidian source rows", () => {
  beforeEach(() => {
    document.body.replaceChildren();
    const root = document.createElement("div");
    root.id = "app";
    root.setAttribute("data-automation-id", ids["settings.window.root"]);
    document.body.append(root);
    app.__test__.reset();
    app.__test__.setRoot(root);
    invokeMock.mockReset();
  });

  it("shows nothing before the source reports", () => {
    render(null);
    expect(bodyText()).not.toContain("Obsidian");
  });

  it("offers found vaults, a folder chooser and the explanation when off", () => {
    render(status({ state: "off", vault: null }), [{ name: "Notes", path: "C:/Notes" }]);
    expect(bodyText()).toContain(
      "the solstone app takes in the notes in one vault on this computer. all of them go into your journal, then each one as it changes. your vault stays as it is.",
    );
    expect(bodyText()).toContain("Notes");
    expect(byId(ids["settings.sources.obsidian.choose"])?.textContent).toBe("choose a folder");
    expect(byId(ids["settings.sources.obsidian.found"])?.textContent).toContain("vaults on this computer");
    expect(document.querySelector('input[type="text"]')).toBeNull();
    invokeMock.mockResolvedValue(null);
    const use = Array.from(document.querySelectorAll("button")).find((b) => b.textContent === "use this vault");
    use?.click();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_use_found_vault", { path: "C:/Notes" });
  });

  it("says when no vault is found", () => {
    render(status({ state: "off", vault: null }));
    expect(bodyText()).toContain("the solstone app didn't find a vault on this computer. choose its folder instead.");
  });

  it("refuses a folder that isn't a vault plainly", () => {
    render(status({ state: "off", vault: null }), [], "not_a_vault");
    expect(byId(ids["settings.sources.obsidian.refusal"])?.textContent).toBe(
      "that folder isn't an Obsidian vault, so nothing was sent.",
    );
  });

  it("warns before the first send and offers continue and cancel", () => {
    render(
      status({
        state: "confirm",
        warning: { vault: "Notes", device: "studio", last_synced_at: "2026-11-08T09:44:33-07:00" },
      }),
    );
    expect(bodyText()).toContain("a vault with this name is already in your journal");
    const warning = byId(ids["settings.sources.obsidian.warning"])?.textContent ?? "";
    expect(warning).toContain("your journal already has a vault named Notes from studio, last updated ");
    expect(warning).toContain("if you continue, changes from both devices go into your journal.");
    expect(byId(ids["settings.sources.obsidian.continue"])?.textContent).toBe("continue");
    expect(byId(ids["settings.sources.obsidian.cancel"])?.textContent).toBe("cancel");
    invokeMock.mockResolvedValue(null);
    byId(ids["settings.sources.obsidian.cancel"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_cancel_warning");
  });

  it("shows no state value during the warning", () => {
    render(status({ state: "confirm", warning: { vault: "Notes", device: null, last_synced_at: null } }));
    expect(byId(ids["settings.sources.obsidian.state"])?.textContent).toBe("");
  });

  it("re-reads the source while it's busy and renders the answer", async () => {
    render(status({ state: "on", waiting: 0 }));
    invokeMock.mockResolvedValue({ v: 1, status: status({ state: "needs_attention", attention: "storage" }) });
    await app.__test__.pollObsidianStatus();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_status");
    expect(byId(ids["settings.sources.obsidian.state"])?.textContent).toBe("needs attention");
  });

  it("doesn't poll while the source is off", async () => {
    render(status({ state: "off", vault: null }));
    invokeMock.mockResolvedValue(null);
    await app.__test__.pollObsidianStatus();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("falls back when the device or date is unknown", () => {
    render(status({ state: "confirm", warning: { vault: "Notes", device: null, last_synced_at: null } }));
    expect(byId(ids["settings.sources.obsidian.warning"])?.textContent).toBe(
      "your journal already has a vault named Notes from another device. if you continue, changes from both devices go into your journal.",
    );
  });

  it("shows progress, waiting, not-on-device and too-large while on", () => {
    render(
      status({
        state: "on",
        waiting: 12,
        first_sync: { found: 40, sent: 3 },
        not_on_device: 2,
        too_large: 1,
      }),
    );
    const body = bodyText();
    expect(body).toContain("3 of 40 notes in your journal");
    expect(body).toContain("12 waiting to sync");
    expect(body).not.toContain("waiting12 waiting");
    expect(body).toContain("not on this device");
    expect(body).toContain("too large to bring in");
    expect(byId(ids["settings.sources.obsidian.state"])?.textContent).toBe("on");
    expect(body).toContain(
      "turning this off stops bringing in changes. what's already in your journal stays there, and what's waiting stays on this computer until you turn it on again.",
    );
    invokeMock.mockResolvedValue(null);
    byId(ids["settings.sources.obsidian.turnOff"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_turn_off");
  });

  it("names the journal update state as waiting, not a fault", () => {
    render(status({ state: "waiting_for_journal_update", waiting: 4 }));
    expect(byId(ids["settings.sources.obsidian.state"])?.textContent).toBe("waiting for a journal update");
    expect(bodyText()).toContain("your journal needs an update before it can take these notes.");
  });

  it("shows needs attention with the reason when a vault can't be read", () => {
    render(status({ state: "needs_attention", attention: "vault_unreadable" }));
    expect(byId(ids["settings.sources.obsidian.state"])?.textContent).toBe("needs attention");
    expect(byId(ids["settings.sources.obsidian.attention"])?.textContent).toContain("can't read this vault");
    render(status({ state: "needs_attention", attention: "file_refused" }));
    expect(byId(ids["settings.sources.obsidian.attention"])?.textContent).toBe(
      "some notes couldn't go into your journal. the solstone app tries them again the next time it starts.",
    );
  });

  function renderHome(value: Status, appState: "idle" | "paused"): HTMLButtonElement | undefined {
    const dump = { ...observingDump(), app_state: appState };
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setObsidian(value);
    app.__test__.renderSettings(dump);
    return Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent === "pause" || b.textContent === "resume",
    );
  }

  it("lets an owner using only Obsidian pause and resume from home", () => {
    invokeMock.mockResolvedValue(null);
    const pause = renderHome(status({ state: "on" }), "idle");
    expect(pause?.textContent).toBe("pause");
    expect(pause?.disabled).toBe(false);
    pause?.click();
    expect(invokeMock).toHaveBeenCalledWith("pause", { reason: "operator", durationSecs: null });

    const resume = renderHome(status({ state: "paused" }), "paused");
    expect(resume?.textContent).toBe("resume");
    resume?.click();
    expect(invokeMock).toHaveBeenCalledWith("resume");
  });

  it("leaves pause off for an idle owner with no source running", () => {
    expect(renderHome(status({ state: "off", vault: null }), "idle")?.disabled).toBe(true);
    expect(renderHome(null, "idle")?.disabled).toBe(true);
  });

  function renderDesk(value: Status): void {
    const base = observingDump();
    const dump = {
      ...base,
      setup: {
        sequence_enabled: true,
        mic: false,
        system_audio: false,
        screen: false,
        start_at_login: false,
        wizard: "done",
        notice_dismissed: true,
        desktop_activity: false,
        desktop_activity_allowed: true,
        desktop_activity_through_pause: false,
        desktop_activity_window_titles: true,
        desktop_activity_page_addresses: true,
      },
    } as typeof base;
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);
    app.__test__.setObsidian(value);
    app.__test__.openDeskSource("obsidian");
    app.__test__.renderSettings(dump);
  }

  const pane = (): HTMLElement => document.querySelector(".desk-pane") as HTMLElement;
  const reasons = (): number => pane().querySelectorAll(".desk-reason").length;
  const facts = (): number => pane().querySelectorAll(".desk-fact").length;

  it("asks on the desk before the first send, and continue and cancel answer it", () => {
    invokeMock.mockResolvedValue(null);
    renderDesk(status({ state: "confirm", vault: null, warning: { vault: "Notes", device: "studio", last_synced_at: null } }));
    expect(document.querySelector(".desk-shell")).not.toBeNull();
    expect(pane().contains(byId(ids["settings.sources.obsidian.warning"]))).toBe(true);
    byId(ids["settings.sources.obsidian.continue"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_continue_warning");
    renderDesk(status({ state: "confirm", vault: null, warning: { vault: "Notes", device: null, last_synced_at: null } }));
    byId(ids["settings.sources.obsidian.cancel"])?.click();
    expect(invokeMock).toHaveBeenCalledWith("obsidian_cancel_warning");
  });

  it("shows the first send, what waits and what stays out on the desk", () => {
    renderDesk(status({ state: "on" }));
    const quiet = { reasons: reasons(), facts: facts() };
    renderDesk(status({ state: "on", waiting: 12, first_sync: { found: 40, sent: 3 }, not_on_device: 2, too_large: 1 }));
    expect(reasons()).toBe(quiet.reasons + 2);
    expect(facts()).toBe(quiet.facts + 2);
  });

  it("gives the journal update its reason on the desk", () => {
    renderDesk(status({ state: "on" }));
    const quiet = reasons();
    renderDesk(status({ state: "waiting_for_journal_update" }));
    expect(reasons()).toBe(quiet + 1);
  });

  it("uses no forbidden owner words", () => {
    render(status({ state: "on", waiting: 1, first_sync: { found: 2, sent: 1 }, not_on_device: 1, too_large: 1 }));
    const text = bodyText().toLowerCase();
    for (const word of ["capture", "record", "watch", "monitor", "track", "collect", "server", "service", "observer"]) {
      expect(text.includes(word)).toBe(false);
    }
  });
});
