// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump } from "./fixtures";

const invokeMock = vi.mocked(invoke);
const ids = automationContract.automation_ids as Record<string, string>;
const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function present(id: string): HTMLElement {
  const el = byId(id);
  expect(el).not.toBeNull();
  return el as HTMLElement;
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

describe("setup and source transitions", () => {
  beforeEach(() => {
    resetRoot();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined as never);
  });

  describe("source switch data-status attribute transitions", () => {
    it("reflects active as on", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [{ kind: "mic" as const, status: "active" as const, device: "USB Mic" }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("on");
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("omits data-status when report is inactive", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [{ kind: "mic" as const, status: "inactive" as const, device: null }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.hasAttribute("data-status")).toBe(false);
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("reflects pause as paused", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        pause: {
          reason: "operator",
          until: null,
          remaining_secs: 300,
        },
        sources: [{ kind: "mic" as const, status: "active" as const, device: "USB Mic" }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("paused");
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("reflects no_input_device as unavailable", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [{ kind: "mic" as const, status: "no_input_device" as const, device: null }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("unavailable");
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("reflects access_denied as access_denied", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [
          {
            kind: "mic" as const,
            status: "faulted" as const,
            reason: "access_denied",
            device: null,
          },
        ],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("unavailable");
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("reflects other faulted status as error", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [
          {
            kind: "mic" as const,
            status: "faulted" as const,
            reason: "device_lost",
            device: null,
          },
        ],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("error");
      expect((el as HTMLInputElement).checked).toBe(true);
    });

    it("reflects disabled choice as off", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: false,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: true,
        },
        sources: [{ kind: "mic" as const, status: "active" as const, device: "USB Mic" }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const el = present(ids["settings.sources.mic.enabled"]);
      expect(el.getAttribute("data-status")).toBe("off");
      expect((el as HTMLInputElement).checked).toBe(false);
    });

    it("reflects status transitions on the wizard switch", () => {
      const activeDump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
        sources: [{ kind: "mic" as const, status: "active" as const, device: "USB Mic" }],
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(activeDump);

      const activeEl = present("settings.setup.mic");
      expect(activeEl.getAttribute("data-status")).toBe("on");
      expect((activeEl as HTMLInputElement).checked).toBe(true);

      const offDump = {
        ...activeDump,
        setup: {
          ...activeDump.setup,
          mic: false,
        },
      };
      app.__test__.renderSettings(offDump);

      const offEl = present("settings.setup.mic");
      expect(offEl.getAttribute("data-status")).toBe("off");
      expect((offEl as HTMLInputElement).checked).toBe(false);
    });
  });

  describe("Next / Back / dismiss never mutate source choices", () => {
    it("advances with Next without mutating source choices", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const nextBtn = present(ids["settings.setup.next"]);
      nextBtn.click();

      expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", { step: "system_audio" });
      expect(invokeMock).not.toHaveBeenCalledWith("set_source_enabled", expect.anything());
    });

    it("goes back with Back without mutating source choices", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "system_audio" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const backBtn = present(ids["settings.setup.back"]);
      backBtn.click();

      expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", { step: "mic" });
      expect(invokeMock).not.toHaveBeenCalledWith("set_source_enabled", expect.anything());
    });

    it("dismisses notice without mutating source choices", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "done" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const dismissBtn = present(ids["settings.home.sourcesNotice.dismiss"]);
      dismissBtn.click();

      expect(invokeMock).toHaveBeenCalledWith("dismiss_notice");
      expect(invokeMock).not.toHaveBeenCalledWith("set_source_enabled", expect.anything());
    });
  });

  describe("Not now leaves current source false and advances", () => {
    it("disables mic and advances to system_audio", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const notNowBtn = present(ids["settings.setup.notNow"]);
      notNowBtn.click();
      await Promise.resolve();
      await Promise.resolve();

      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "mic",
        enabled: false,
      });
      expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", {
        step: "system_audio",
      });
    });

    it("disables system_audio and advances to screen", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: false,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "system_audio" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const notNowBtn = present(ids["settings.setup.notNow"]);
      notNowBtn.click();
      await Promise.resolve();
      await Promise.resolve();

      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "system_audio",
        enabled: false,
      });
      expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", {
        step: "screen",
      });
    });

    it("disables screen and advances to connection", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: false,
          system_audio: false,
          screen: true,
          start_at_login: false,
          wizard: "screen" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const notNowBtn = present(ids["settings.setup.notNow"]);
      notNowBtn.click();
      await Promise.resolve();
      await Promise.resolve();

      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "screen",
        enabled: false,
      });
      expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", {
        step: "connection",
      });
    });

    it("clicks pauseSource on mic step and turns off source without global pause", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const pauseBtn = present(ids["settings.setup.pauseSource"]);
      pauseBtn.click();
      await Promise.resolve();

      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "mic",
        enabled: false,
      });
      expect(invokeMock).not.toHaveBeenCalledWith("pause", expect.anything());
    });
  });

  describe("proof nodes reflect withheld vs ready", () => {
    it("reflects withheld when preview is empty", () => {
      app.__test__.setPreview({});
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const micProof = present(ids["settings.setup.mic.proof"]);
      expect(micProof.getAttribute("data-proof")).toBe("withheld");
      expect(micProof.hasAttribute("data-level")).toBe(false);
    });

    it("reflects ready with level 0", () => {
      app.__test__.setPreview({ mic: 0 });
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const micProof = present(ids["settings.setup.mic.proof"]);
      expect(micProof.getAttribute("data-proof")).toBe("ready");
      expect(micProof.getAttribute("data-level")).toBe("0");
    });

    it("reflects ready with positive numeric level", () => {
      app.__test__.setPreview({ mic: 0.75 });
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const micProof = present(ids["settings.setup.mic.proof"]);
      expect(micProof.getAttribute("data-proof")).toBe("ready");
      expect(micProof.getAttribute("data-level")).toBe("0.75");
    });

    it("reflects ready for system audio with level", () => {
      app.__test__.setPreview({ system_audio: 0.42 });
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "system_audio" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const sysProof = present(ids["settings.setup.systemAudio.proof"]);
      expect(sysProof.getAttribute("data-proof")).toBe("ready");
      expect(sysProof.getAttribute("data-level")).toBe("0.42");
    });

    it("reflects screen proof ready vs withheld", () => {
      const dump = {
        ...observingDump(),
        setup: {
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "screen" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");

      app.__test__.setPreview({ screen: false });
      app.__test__.renderSettings(dump);
      const withheld = present(ids["settings.setup.screen.proof"]);
      expect(withheld.getAttribute("data-proof")).toBe("withheld");

      app.__test__.setPreview({ screen: true });
      app.__test__.renderSettings(dump);
      const readyBool = present(ids["settings.setup.screen.proof"]);
      expect(readyBool.getAttribute("data-proof")).toBe("ready");

      app.__test__.setPreview({ screen_frame: { width: 1920, height: 1080, rgba_base64: "" } });
      app.__test__.renderSettings(dump);
      const readyFrame = present(ids["settings.setup.screen.proof"]);
      expect(readyFrame.getAttribute("data-proof")).toBe("ready");
    });
  });
});
