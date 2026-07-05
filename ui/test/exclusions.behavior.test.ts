// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { invoke } from "@tauri-apps/api/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { exclusionRules, exclusionsDump } from "./fixtures";

const ids = automationContract.automation_ids;
const WARNING_TEXT =
  "These rules are active now but couldn't be saved — they may not survive a restart.";

type ExclusionRulesFixture = ReturnType<typeof exclusionRules>;
type RunningAppFixture = { exe_name: string; display_name: string };

const invokeMock = vi.mocked(invoke);

function byId(id: string): HTMLElement | null {
  return document.querySelector(`[data-automation-id="${id}"]`);
}

function present<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = byId(id);
  expect(el).not.toBeNull();
  return el as T;
}

function resetRoot(): HTMLDivElement {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  rootEl.setAttribute("data-automation-id", ids["settings.window.root"]);
  document.body.append(rootEl);
  app.__test__.reset();
  app.__test__.setRoot(rootEl);
  return rootEl;
}

function renderPrivacy(
  rules: ExclusionRulesFixture,
  runningApps: RunningAppFixture[] = [],
): void {
  const dump = exclusionsDump();
  app.__test__.setRoute("privacy");
  app.__test__.setHealth(dump);
  app.__test__.setExclusions(rules);
  app.__test__.setRunningApps(runningApps);
  app.__test__.renderSettings(dump);
}

function chipLabels(listId: string): string[] {
  return Array.from(present(listId).children).map(
    (chip) => chip.querySelector("span")?.textContent ?? chip.textContent ?? "",
  );
}

function setTitleDraft(value: string): void {
  const input = present<HTMLInputElement>(ids["settings.exclusions.titleInput"]);
  input.value = value;
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function clickTitleAdd(): void {
  present<HTMLButtonElement>(ids["settings.exclusions.titleAdd"]).click();
}

function findRemoveButton(listId: string, value: string): HTMLButtonElement {
  const button = present(listId).querySelector<HTMLButtonElement>(
    `button[aria-label="remove ${value}"]`,
  );
  expect(button).not.toBeNull();
  return button as HTMLButtonElement;
}

async function flushAsync(): Promise<void> {
  await vi.runAllTimersAsync();
  await Promise.resolve();
}

describe("exclusions behavior", () => {
  beforeEach(() => {
    vi.useFakeTimers({
      toFake: ["setTimeout", "clearTimeout", "Date"],
    });
    invokeMock.mockReset();
    resetRoot();
  });

  afterEach(() => {
    vi.useRealTimers();
    invokeMock.mockReset();
  });

  it("renders settled exclusion rules from the refetch", async () => {
    const initial = exclusionRules({ titles: ["existing"] });
    const authoritative = {
      excluded_exes: [],
      title_patterns: ["banking", "refetched-sentinel"],
      exclude_private_browsing: true,
    };
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(authoritative);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    setTitleDraft("  Banking  ");
    clickTitleAdd();
    await flushAsync();

    expect(chipLabels(ids["settings.exclusions.titlesList"])).toEqual([
      "banking",
      "refetched-sentinel",
    ]);
  });

  it("reverts to authoritative rules after set_exclusions fails", async () => {
    const initial = exclusionRules({ titles: ["keep"] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.reject(new Error("boom"));
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    setTitleDraft("phantom");
    clickTitleAdd();
    await flushAsync();

    expect(chipLabels(ids["settings.exclusions.titlesList"])).toEqual(["keep"]);
    expect(document.querySelectorAll('[aria-busy="true"]')).toHaveLength(0);
    expect(
      present<HTMLButtonElement>(ids["settings.exclusions.titleAdd"]).disabled,
    ).toBe(false);
  });

  it("marks the private-browsing toggle pending synchronously", async () => {
    const initial = exclusionRules();
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    present<HTMLInputElement>(ids["settings.exclusions.privateBrowsing"]).click();

    const busy = present<HTMLInputElement>(ids["settings.exclusions.privateBrowsing"]);
    expect(busy.disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    await flushAsync();
    const settled = present<HTMLInputElement>(ids["settings.exclusions.privateBrowsing"]);
    expect(settled.disabled).toBe(false);
    expect(settled.hasAttribute("aria-busy")).toBe(false);
  });

  it("marks app add pending synchronously", async () => {
    const initial = exclusionRules({ exes: [], titles: [] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial, [{ exe_name: "browser.exe", display_name: "Browser" }]);

    present<HTMLSelectElement>(ids["settings.exclusions.appInput"]).value = "browser.exe";
    present<HTMLButtonElement>(ids["settings.exclusions.appAdd"]).click();

    const busy = present<HTMLButtonElement>(ids["settings.exclusions.appAdd"]);
    expect(busy.disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    await flushAsync();
    const settled = present<HTMLButtonElement>(ids["settings.exclusions.appAdd"]);
    expect(settled.disabled).toBe(false);
    expect(settled.hasAttribute("aria-busy")).toBe(false);
  });

  it("marks title add pending synchronously", async () => {
    const initial = exclusionRules({ titles: [] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    setTitleDraft("banking");
    clickTitleAdd();

    const busy = present<HTMLButtonElement>(ids["settings.exclusions.titleAdd"]);
    expect(busy.disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    await flushAsync();
    const settled = present<HTMLButtonElement>(ids["settings.exclusions.titleAdd"]);
    expect(settled.disabled).toBe(false);
    expect(settled.hasAttribute("aria-busy")).toBe(false);
  });

  it("marks app remove pending synchronously", async () => {
    const initial = exclusionRules({ exes: ["secret.exe"], titles: [] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    findRemoveButton(ids["settings.exclusions.appsList"], "secret.exe").click();

    const busy = findRemoveButton(ids["settings.exclusions.appsList"], "secret.exe");
    expect(busy.disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    await flushAsync();
    const settled = findRemoveButton(ids["settings.exclusions.appsList"], "secret.exe");
    expect(settled.disabled).toBe(false);
    expect(settled.hasAttribute("aria-busy")).toBe(false);
  });

  it("marks title remove pending synchronously", async () => {
    const initial = exclusionRules({ titles: ["banking"] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    findRemoveButton(ids["settings.exclusions.titlesList"], "banking").click();

    const busy = findRemoveButton(ids["settings.exclusions.titlesList"], "banking");
    expect(busy.disabled).toBe(true);
    expect(busy.getAttribute("aria-busy")).toBe("true");
    await flushAsync();
    const settled = findRemoveButton(ids["settings.exclusions.titlesList"], "banking");
    expect(settled.disabled).toBe(false);
    expect(settled.hasAttribute("aria-busy")).toBe(false);
  });

  it("shows and clears the persist warning based on successful outcomes", async () => {
    const initial = exclusionRules({ titles: [] });
    let persisted = false;
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial);

    setTitleDraft("banking");
    clickTitleAdd();
    await flushAsync();

    expect(
      Array.from(document.querySelectorAll("div")).some(
        (el) => el.textContent === WARNING_TEXT,
      ),
    ).toBe(true);

    persisted = true;
    setTitleDraft("medical");
    clickTitleAdd();
    await flushAsync();

    expect(
      Array.from(document.querySelectorAll("div")).some(
        (el) => el.textContent === WARNING_TEXT,
      ),
    ).toBe(false);
  });

  it("keeps the store-app exclusion keyed on applicationframehost.exe", () => {
    const initial = exclusionRules({ exes: [], titles: [] });
    invokeMock.mockImplementation((cmd) => {
      if (cmd === "set_exclusions") return Promise.resolve({ persisted: true });
      if (cmd === "get_exclusions") return Promise.resolve(initial);
      return Promise.resolve(null);
    });
    renderPrivacy(initial, [
      { exe_name: "applicationframehost.exe", display_name: "Some Window" },
    ]);

    present<HTMLSelectElement>(ids["settings.exclusions.appInput"]).value =
      "applicationframehost.exe";
    present<HTMLButtonElement>(ids["settings.exclusions.appAdd"]).click();

    const setCall = invokeMock.mock.calls.find(([cmd]) => cmd === "set_exclusions");
    expect(setCall?.[1]).toMatchObject({
      rules: {
        excluded_exes: expect.arrayContaining(["applicationframehost.exe"]),
      },
    });
  });
});
