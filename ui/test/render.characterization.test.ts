// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it } from "vitest";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import {
  exclusionRules,
  exclusionsDump,
  faultedSourceDump,
  micDeviceList,
  micView,
  notPairedDump,
  observingDump,
  pausedDump,
  updateView,
  type UpdateDisplayKind,
} from "./fixtures";

const ids = automationContract.automation_ids;
const STORE_APPS_LABEL =
  "Store apps (all) — Windows Store apps share one entry; excluding it excludes them all";
const EXCLUSION_BOUNDARY =
  "A window that closes or moves can appear for up to one frame before exclusion applies.";
const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function resetRoot(rootId = ids["settings.window.root"]): HTMLDivElement {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  rootEl.setAttribute("data-automation-id", rootId);
  document.body.append(rootEl);
  app.__test__.reset();
  app.__test__.setRoot(rootEl);
  return rootEl;
}

function present(id: string): HTMLElement {
  const el = byId(id);
  expect(el).not.toBeNull();
  return el as HTMLElement;
}

function absent(id: string): void {
  expect(byId(id)).toBeNull();
}

describe("settings renderer characterization", () => {
  beforeEach(() => {
    resetRoot();
  });

  it("renders the observing home emitted ids", () => {
    const dump = observingDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    present(ids["settings.window.root"]);
    expect(present(ids["settings.status.appState.state"]).textContent).toBe("on");
  });

  it("renders the kinship intro on not-paired home", () => {
    const dump = notPairedDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    const block = present(ids["settings.home.kinship"]);
    expect(block.textContent).toContain("this is sol, part of solstone.");
    expect(block.textContent).toContain(
      "sol lives on your devices, experiences your day with you, and keeps it all in your journal.",
    );
    expect(block.textContent).toContain("your journal is always private, only yours.");
  });

  it("omits the kinship intro once paired", () => {
    const dump = observingDump();
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    absent(ids["settings.home.kinship"]);
  });

  it.each(["pairing", "failed"] as const)(
    "omits the kinship intro while %s",
    (phase) => {
      const base = notPairedDump();
      const dump = {
        ...base,
        sync: { ...base.sync, pairing: { ...base.sync.pairing, phase } },
      };
      app.__test__.setRoute("home");
      app.__test__.setHealth(dump);

      app.__test__.renderSettings(dump);

      absent(ids["settings.home.kinship"]);
    },
  );

  it("renders the observing sources emitted ids", () => {
    const dump = observingDump();
    app.__test__.setRoute("sources");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    present(ids["settings.sources.screen.state"]);
    present(ids["settings.sources.systemAudio.state"]);
    present(ids["settings.sources.mic.state"]);
  });

  it("renders a bounded pause state on home", () => {
    const dump = pausedDump(900);
    app.__test__.setRoute("home");
    app.__test__.setHealth(dump, 1_700_000_000);

    app.__test__.renderSettings(dump);

    expect(present(ids["settings.status.appState.state"]).textContent).toContain(
      "paused — 15 min left",
    );
  });

  it("renders not-paired journal state", () => {
    const dump = notPairedDump();
    app.__test__.setRoute("journal");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    present(ids["settings.pairing.state"]);
    present(ids["settings.pairing.journal"]);
    present(ids["settings.pairing.input"]);
    present(ids["settings.pairing.submit"]);
    expect(present(ids["settings.status.upload.state"]).textContent).toContain("not paired");
  });

  it("renders a faulted required source with its detail", () => {
    const dump = faultedSourceDump();
    app.__test__.setRoute("sources");
    app.__test__.setHealth(dump);

    app.__test__.renderSettings(dump);

    expect(present(ids["settings.sources.screen.state"]).textContent).toContain(
      "attention needed: screen denied",
    );
    present(ids["settings.sources.systemAudio.state"]);
    present(ids["settings.sources.mic.state"]);
  });

  it("renders exclusions chips and filters the app picker", () => {
    const dump = exclusionsDump();
    const rules = exclusionRules({ exes: ["secret.exe", "notes.exe"], titles: ["banking", "medical"] });
    app.__test__.setRoute("privacy");
    app.__test__.setHealth(dump);
    app.__test__.setExclusions(rules);
    app.__test__.setRunningApps([
      { exe_name: "solstone-windows-app.exe", display_name: "solstone" },
      { exe_name: "secret.exe", display_name: "Secret" },
      { exe_name: "browser.exe", display_name: "Browser" },
    ]);

    app.__test__.renderSettings(dump);

    const apps = present(ids["settings.exclusions.appsList"]);
    const titles = present(ids["settings.exclusions.titlesList"]);
    expect(apps.children).toHaveLength(2);
    expect(titles.children).toHaveLength(2);
    for (const chip of Array.from(apps.children)) {
      expect(chip.querySelector("button")?.textContent).toBe("×");
    }
    for (const chip of Array.from(titles.children)) {
      expect(chip.querySelector("button")?.textContent).toBe("×");
    }

    const select = present(ids["settings.exclusions.appInput"]) as HTMLSelectElement;
    const options = Array.from(select.options).map((option) => option.value);
    expect(options).toContain("browser.exe");
    expect(options).not.toContain("solstone-windows-app.exe");
    expect(options).not.toContain("secret.exe");
    present(ids["settings.exclusions.activity"]);
  });

  it("renders store-app label, boundary caption, and unchanged exclusion activity", () => {
    const dump = exclusionsDump();
    const rules = exclusionRules({ exes: [], titles: [] });
    app.__test__.setRoute("privacy");
    app.__test__.setHealth(dump);
    app.__test__.setExclusions(rules);
    app.__test__.setRunningApps([
      { exe_name: "applicationframehost.exe", display_name: "Some Window" },
    ]);

    app.__test__.renderSettings(dump);

    const select = present(ids["settings.exclusions.appInput"]) as HTMLSelectElement;
    const storeOption = Array.from(select.options).find(
      (option) => option.value === "applicationframehost.exe",
    );
    expect(storeOption?.textContent).toBe(STORE_APPS_LABEL);
    expect(storeOption?.value).toBe("applicationframehost.exe");
    expect(
      Array.from(document.querySelectorAll("div")).some(
        (el) => el.textContent === EXCLUSION_BOUNDARY,
      ),
    ).toBe(true);
    expect(present(ids["settings.exclusions.activity"]).textContent).toBe(
      "3 frames kept out of your journal this session · 1 dropped",
    );
  });

  it("renders one microphone row per ordered device", () => {
    const dump = observingDump();
    app.__test__.setRoute("sources");
    app.__test__.setHealth(dump);
    app.__test__.setMic(micView());
    app.__test__.setMicDevices(micDeviceList());

    app.__test__.renderSettings(dump);

    const list = present(ids["settings.mic.devices"]);
    const rows = Array.from(list.children) as HTMLElement[];
    expect(rows).toHaveLength(3);
    expect(rows.map((row) => row.textContent)).toEqual([
      "↑↓USB Mic · activedisable",
      "↑↓Array Micenable",
      "↑↓Webcam Micdisable",
    ]);
    const disabledLabel = Array.from(rows[1].querySelectorAll("span")).find((span) =>
      span.textContent?.startsWith("Array Mic"),
    ) as HTMLElement;
    expect(disabledLabel.style.textDecoration).toBe("line-through");
    expect(rows[0].textContent).toContain(" · active");
    present(ids["settings.mic.active"]);
    expect(present(ids["settings.mic.gain"]).querySelectorAll("button")).toHaveLength(4);
  });

  it.each([
    ["never_checked", ["state", "checkNow", "autoCheck", "frequency", "autoDownload"], ["lastChecked", "notes", "download", "install", "retry"]],
    ["available", ["state", "lastChecked", "notes", "checkNow", "download", "autoCheck", "frequency", "autoDownload"], ["install", "retry"]],
    ["downloading", ["state", "autoCheck", "frequency", "autoDownload"], ["lastChecked", "notes", "checkNow", "download", "install", "retry"]],
    ["staged", ["state", "checkNow", "install", "autoCheck", "frequency", "autoDownload"], ["lastChecked", "notes", "download", "retry"]],
    ["failed", ["state", "lastChecked", "checkNow", "retry", "autoCheck", "frequency", "autoDownload"], ["notes", "download", "install"]],
  ] as Array<[UpdateDisplayKind, string[], string[]]>)(
    "renders the %s updates emitted-id subset",
    (display, expected, excluded) => {
      const dump = observingDump();
      app.__test__.setRoute("updates");
      app.__test__.setHealth(dump);
      app.__test__.setUpdate(updateView(display));

      app.__test__.renderSettings(dump);

      for (const key of expected) {
        present(ids[`settings.updates.${key}`]);
      }
      for (const key of excluded) {
        absent(ids[`settings.updates.${key}`]);
      }
      absent(ids["settings.updates.cancel"]);
    },
  );

  it("renders the about surface with its emitted ids", () => {
    const dump = observingDump();
    resetRoot(ids["about.window.root"]);
    app.__test__.setLabel("about");
    app.__test__.setHealth(dump);

    app.__test__.renderAbout(dump);

    present(ids["about.window.root"]);
    expect(present(ids["about.version"]).textContent).toBe(dump.version);
  });
});
