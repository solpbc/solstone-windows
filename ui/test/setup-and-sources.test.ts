// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc
// @vitest-environment jsdom

import "./setup";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { observingDump, sampleMarkSpec } from "./fixtures";

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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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
          sequence_enabled: true,
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

    it("turns a source off from its own switch, with no separate off button, and no global pause", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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

      const step = present(ids["settings.setup.step"]);
      expect(step.querySelector('[data-automation-id="settings.setup.pauseSource"]')).toBeNull();
      const actions = Array.from(step.querySelectorAll("button")).map((b) => b.dataset.automationId);
      expect(actions).toEqual([ids["settings.setup.notNow"], ids["settings.setup.next"]]);

      const sw = present("settings.setup.mic") as HTMLInputElement;
      expect(sw.checked).toBe(true);
      sw.checked = false;
      sw.dispatchEvent(new Event("change"));
      await Promise.resolve();

      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "mic",
        enabled: false,
      });
      expect(invokeMock).not.toHaveBeenCalledWith("pause", expect.anything());
    });

    it("says what goes into the journal on source steps, not on the connect step", () => {
      const base = {
        sequence_enabled: true,
        mic: false,
        system_audio: false,
        screen: false,
        start_at_login: false,
        notice_dismissed: false,
      };
      for (const wizard of ["mic", "system_audio", "screen"] as const) {
        resetRoot();
        app.__test__.setRoute("home");
        app.__test__.renderSettings({ ...observingDump(), setup: { ...base, wizard } });
        expect(present(ids["settings.setup.step"]).querySelector(".wizard-sentence")).not.toBeNull();
      }
      resetRoot();
      app.__test__.setRoute("home");
      app.__test__.renderSettings({ ...observingDump(), setup: { ...base, wizard: "connection" as const } });
      expect(present(ids["settings.setup.step"]).querySelector(".wizard-sentence")).toBeNull();
    });
  });

  describe("proof nodes and preview paint rules", () => {
    it("renders meter aria-valuenow for 0 and positive mic_level", () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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
      app.__test__.setAcceptedProofGeneration(1);

      app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0 });
      const micProof = present(ids["settings.setup.mic.proof"]);
      const meter0 = micProof.querySelector('[role="meter"]');
      expect(meter0).not.toBeNull();
      expect(meter0?.getAttribute("aria-valuenow")).toBe("0");

      app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.75 });
      const meterPos = micProof.querySelector('[role="meter"]');
      expect(meterPos).not.toBeNull();
      expect(meterPos?.getAttribute("aria-valuenow")).toBe("0.75");
    });

    it("renders canvas size and putImageData bytes from rgba number array", () => {
      let putImageDataArgs: any = null;
      const fakeCtx = {
        createImageData: (w: number, h: number) => ({
          width: w,
          height: h,
          data: new Uint8ClampedArray(w * h * 4),
        }),
        putImageData: (imgData: any, x: number, y: number) => {
          putImageDataArgs = { imgData, x, y };
        },
      };
      vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(fakeCtx as any);

      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "screen" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);
      app.__test__.setAcceptedProofGeneration(1);

      const rgba = [255, 128, 64, 255];
      app.__test__.applyPreview({
        consumer_generation: 1,
        screen_frame: { width: 320, height: 240, rgba },
      });

      const screenProof = present(ids["settings.setup.screen.proof"]);
      const canvas = screenProof.querySelector("canvas");
      expect(canvas).not.toBeNull();
      expect(canvas?.width).toBe(320);
      expect(canvas?.height).toBe(240);
      expect(putImageDataArgs).not.toBeNull();
      expect(putImageDataArgs.imgData.data[0]).toBe(255);
      expect(putImageDataArgs.imgData.data[1]).toBe(128);
      expect(putImageDataArgs.imgData.data[2]).toBe(64);
      expect(putImageDataArgs.imgData.data[3]).toBe(255);
    });

    it("renders visible waiting and off", () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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
      expect(micProof.textContent).toContain("waiting");

      const offDump = {
        ...dump,
        setup: {
          sequence_enabled: true,
          ...dump.setup,
          mic: false,
        },
      };
      app.__test__.renderSettings(offDump);
      // The control's own state already reads off; the proof area stays empty.
      const withheldProof = present(ids["settings.setup.mic.proof"]);
      expect(withheldProof.childElementCount).toBe(0);
      expect(withheldProof.textContent).toBe("");
    });

    it("renders unavailable/error token plus detail, leaves not-now enabled, and retry inside proof invokes set_source_enabled", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
        sources: [
          {
            kind: "mic" as const,
            status: "no_input_device" as const,
            detail: "no input mic found",
            device: null,
          },
        ],
      };
      app.__test__.setRoute("home");
      app.__test__.renderSettings(dump);

      const micProof = present(ids["settings.setup.mic.proof"]);
      expect(micProof.textContent).toContain("unavailable");
      expect(micProof.textContent).toContain("no microphone is available");

      const notNowBtn = present(ids["settings.setup.notNow"]) as HTMLButtonElement;
      expect(notNowBtn.disabled).toBe(false);

      const retryBtn = micProof.querySelector("button");
      expect(retryBtn).not.toBeNull();
      expect(retryBtn?.textContent).toBe("retry");

      retryBtn?.click();
      await Promise.resolve();
      expect(invokeMock).toHaveBeenCalledWith("set_source_enabled", {
        kind: "mic",
        enabled: true,
      });
    });
  });

  describe("accessible names on source checkboxes", () => {
    it("provides accessible names for microphone, system audio, and screen", () => {
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
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const micEl = present(ids["settings.sources.mic.enabled"]) as HTMLInputElement;
      const sysEl = present(ids["settings.sources.systemAudio.enabled"]) as HTMLInputElement;
      const screenEl = present(ids["settings.sources.screen.enabled"]) as HTMLInputElement;

      const getAccessibleName = (el: HTMLElement) =>
        (el.getAttribute("aria-label") || el.closest("label")?.textContent || "").toLowerCase();

      expect(getAccessibleName(micEl)).toContain("microphone");
      expect(getAccessibleName(sysEl)).toContain("system audio");
      expect(getAccessibleName(screenEl)).toContain("screen");
    });
  });

  describe("status text and save failure", () => {
    it("renders visible status text on rather than active", () => {
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
        sources: [{ kind: "mic" as const, status: "active" as const, device: "USB Mic" }],
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const stateEl = present(ids["settings.sources.mic.state"]);
      expect(stateEl.textContent).toContain("on");
      expect(stateEl.textContent).not.toContain("active");
    });

    it("renders non-empty .save-failure without asserting its sentence", () => {
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
          failed_sources: [{ kind: "mic" as const, attempted: true }],
        },
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const failureEl = document.querySelector(".save-failure");
      expect(failureEl).not.toBeNull();
      expect((failureEl?.textContent ?? "").trim().length).toBeGreaterThan(0);
    });

    it("save retry button invokes retry_save", async () => {
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
          failed_sources: [{ kind: "mic" as const, attempted: true }],
        },
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(dump);

      const retryBtn = present(ids["settings.sources.saveRetry"]);
      retryBtn.click();
      await Promise.resolve();
      expect(invokeMock).toHaveBeenCalledWith("retry_save");
    });
  });

  describe("routes, connection step, and static copy", () => {
    it("shows start-at-login on sources route for wizard done and wizard mic", () => {
      const doneDump = {
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
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(doneDump);
      expect(byId(ids["settings.setup.startAtLogin"])).not.toBeNull();

      const micDump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
          mic: true,
          system_audio: true,
          screen: true,
          start_at_login: false,
          wizard: "mic" as const,
          notice_dismissed: false,
        },
      };
      app.__test__.setRoute("sources");
      app.__test__.renderSettings(micDump);
      expect(byId(ids["settings.setup.startAtLogin"])).not.toBeNull();
    });

    it("renders connection step pairing control for each phase and nav does not invoke set_source_enabled", async () => {
      const phases: Array<{ phase: any; detail?: string }> = [
        { phase: "not_paired" },
        { phase: "pairing" },
        { phase: "awaiting_confirmation" },
        { phase: "failed", detail: "relay_unpaid" },
        { phase: "not_paired", detail: "mark_rejected" },
        { phase: "paired" },
      ];

      for (const p of phases) {
        invokeMock.mockReset();
        invokeMock.mockResolvedValue(undefined as never);

        const dump = {
          ...observingDump(),
          setup: {
          sequence_enabled: true,
            mic: true,
            system_audio: true,
            screen: true,
            start_at_login: false,
            wizard: "connection" as const,
            notice_dismissed: false,
          },
          sync: {
            ...observingDump().sync,
            pairing: {
              phase: p.phase,
              detail: p.detail ?? null,
              mark: p.phase === "paired" ? sampleMarkSpec("liquefy", "smock", "#3b82f6") : null,
              expected_mark: null,
              received_mark: null,
            },
          },
        };

        app.__test__.setRoute("home");
        app.__test__.renderSettings(dump as any);

        const backBtn = present(ids["settings.setup.back"]);
        backBtn.click();
        await Promise.resolve();

        const forwardBtn =
          p.phase === "paired"
            ? present(ids["settings.setup.next"])
            : present(ids["settings.setup.notNow"]);
        forwardBtn.click();
        await Promise.resolve();

        expect(invokeMock).not.toHaveBeenCalledWith("set_source_enabled", expect.anything());
      }
    });

    it("displays notice text exactly: you can turn each source on or off in sources.", () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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

      const noticeEl = present(ids["settings.home.sourcesNotice"]);
      expect(noticeEl.textContent).toContain("you can turn each source on or off in sources.");
    });
  });

  describe("focus retention across applyPreview and wizard step advance", () => {
    it("focuses Next, applies preview with accepted generation, leaving activeElement unchanged and meter updated", () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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
      nextBtn.focus();
      expect(document.activeElement).toBe(nextBtn);

      app.__test__.setAcceptedProofGeneration(1);
      app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.65 });

      expect(document.activeElement).toBe(nextBtn);
      const meter = present(ids["settings.setup.mic.proof"]).querySelector('[role="meter"]');
      expect(meter?.getAttribute("aria-valuenow")).toBe("0.65");
    });

    it("does not change the meter on mismatched consumer_generation", () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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

      app.__test__.setAcceptedProofGeneration(1);
      app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.5 });
      const meter = present(ids["settings.setup.mic.proof"]).querySelector('[role="meter"]');
      expect(meter?.getAttribute("aria-valuenow")).toBe("0.5");

      app.__test__.applyPreview({ consumer_generation: 99, mic_level: 0.99 });
      expect(meter?.getAttribute("aria-valuenow")).toBe("0.5");
    });

    it("introduces the next source heading before its controls when a step change is saved", async () => {
      const dump = {
        ...observingDump(),
        setup: {
          sequence_enabled: true,
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

      const sysDump = {
        ...dump,
        setup: {
          sequence_enabled: true,
          ...dump.setup,
          wizard: "system_audio" as const,
        },
      };

      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "set_wizard_step") {
          return { saved: true, proof_generation: 2 } as any;
        }
        if (cmd === "get_health") {
          return sysDump as any;
        }
        return undefined as any;
      });

      const nextBtn = present(ids["settings.setup.next"]);
      nextBtn.click();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();

      const activeEl = document.activeElement;
      expect(activeEl).toBe(document.querySelector(".wizard-step-title"));
      expect(activeEl?.getAttribute("tabindex")).toBe("-1");
    });
  });
});

describe("setup recovery and proof lifetimes", () => {
  const dump = () => ({
    ...observingDump(),
    setup: {
          sequence_enabled: true,
      mic: true, system_audio: false, screen: false, start_at_login: false,
      wizard: "mic" as const, notice_dismissed: false,
    },
  });
  beforeEach(() => {
    resetRoot();
    app.__test__.setLabel("settings");
    app.__test__.setRoute("home");
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined as never);
  });

  it("rejects delayed old proof while hidden and while a reopen reply is pending", async () => {
    let opens = 0;
    let resolveOpen: ((generation: number) => void) | undefined;
    invokeMock.mockImplementation(async (command, args) => {
      if (command !== "set_proof_consumer") return undefined as never;
      if (!(args as { source?: string })?.source) return 2 as never;
      opens += 1;
      if (opens === 1) return 1 as never;
      return new Promise<number>((resolve) => { resolveOpen = resolve; }) as never;
    });
    app.__test__.renderSettings(dump());
    await app.__test__.proofIdle();
    app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.7 });
    expect(document.querySelector("meter")).not.toBeNull();
    app.__test__.setProofWindowActive(false);
    app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.9 });
    expect(document.querySelector("meter")).toBeNull();
    app.__test__.setProofWindowActive(true);
    await vi.waitFor(() => expect(opens).toBe(2));
    app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.9 });
    expect(document.querySelector("meter")).toBeNull();
    resolveOpen!(3);
    await app.__test__.proofIdle();
    expect(document.querySelector("meter")).toBeNull();
    app.__test__.applyPreview({ consumer_generation: 3, mic_level: 0.4 });
    expect(document.querySelector("meter")?.getAttribute("aria-valuenow")).toBe("0.4");
  });

  it("revalidates a delayed native inactivity notice after focus has returned", async () => {
    const focus = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    let generation = 0;
    invokeMock.mockImplementation(async (command) => command === "set_proof_consumer" ? ++generation as never : undefined as never);
    try {
      app.__test__.renderSettings(dump());
      await app.__test__.proofIdle();
      const first = generation;
      app.__test__.applyPreview({ consumer_generation: first, mic_level: 0.7 });
      expect(document.querySelector("meter")).not.toBeNull();
      app.__test__.handleProofInactive();
      app.__test__.applyPreview({ consumer_generation: first, mic_level: 0.9 });
      expect(document.querySelector("meter")).toBeNull();
      await app.__test__.proofIdle();
      expect(generation).toBeGreaterThan(first);
      expect(invokeMock).toHaveBeenLastCalledWith("set_proof_consumer", { source: "mic" });
      app.__test__.applyPreview({ consumer_generation: first, mic_level: 0.9 });
      expect(document.querySelector("meter")).toBeNull();
      app.__test__.applyPreview({ consumer_generation: generation, mic_level: 0.4 });
      expect(document.querySelector("meter")?.getAttribute("aria-valuenow")).toBe("0.4");
    } finally {
      focus.mockRestore();
    }
  });

  it("can retry native refusal without a second focus event", async () => {
    let attempts = 0;
    invokeMock.mockImplementation(async (command, args) => {
      if (command !== "set_proof_consumer") return undefined as never;
      if (!(args as { source?: string })?.source) return 1 as never;
      attempts += 1;
      if (attempts === 1) throw new Error("proof window is not active");
      return 3 as never;
    });
    app.__test__.renderSettings(dump());
    await app.__test__.proofIdle();
    app.__test__.applyPreview({ consumer_generation: 1, mic_level: 0.9 });
    expect(document.querySelector("meter")).toBeNull();
    app.__test__.renderSettings(dump());
    await app.__test__.proofIdle();
    expect(attempts).toBe(2);
    app.__test__.applyPreview({ consumer_generation: 3, mic_level: 0.4 });
    expect(document.querySelector("meter")?.getAttribute("aria-valuenow")).toBe("0.4");
  });

  it.each(["paused", "off", "unavailable"])("discards cached and queued proof across %s and waits for renewed input", async (state) => {
    let generation = 0;
    invokeMock.mockImplementation(async (command) => command === "set_proof_consumer" ? ++generation as never : undefined as never);
    app.__test__.renderSettings(dump());
    await app.__test__.proofIdle();
    const oldGeneration = generation;
    app.__test__.applyPreview({ consumer_generation: oldGeneration, mic_level: 0.6 });
    expect(document.querySelector("meter")).not.toBeNull();
    const unavailable = state === "paused" ? { ...dump(), app_state: "paused" as const,
      pause: { reason: "operator", until: null, remaining_secs: null } }
      : state === "off" ? { ...dump(), setup: { ...dump().setup, mic: false } }
      : { ...dump(), sources: [{ kind: "mic" as const, status: "no_input_device" as const, device: null }] };
    app.__test__.renderSettings(unavailable);
    app.__test__.applyPreview({ consumer_generation: oldGeneration, mic_level: 0.9 });
    expect(document.querySelector("meter")).toBeNull();
    app.__test__.renderSettings(dump());
    await app.__test__.proofIdle();
    app.__test__.applyPreview({ consumer_generation: oldGeneration, mic_level: 0.9 });
    expect(document.querySelector("meter")).toBeNull();
    app.__test__.applyPreview({ consumer_generation: generation, mic_level: 0.4 });
    expect(document.querySelector("meter")?.getAttribute("aria-valuenow")).toBe("0.4");
  });

  it("shows a failed save immediately while the source switch retains keyboard focus", async () => {
    const initial = dump();
    initial.setup.mic = false;
    const failed = { ...initial, setup: { ...initial.setup,
      failed_sources: [{ kind: "mic" as const, attempted: true }] } };
    invokeMock.mockImplementation(async (command) => command === "get_health" ? failed as never : undefined as never);
    app.__test__.renderSettings(initial);
    const control = document.querySelector<HTMLInputElement>(".settings-wizard input[type=checkbox]")!;
    control.focus();
    control.click();
    await vi.waitFor(() => {
      const current = document.querySelector<HTMLInputElement>(".settings-wizard input[type=checkbox]")!;
      expect(current.checked).toBe(false);
      expect(document.activeElement).toBe(current);
      expect(document.querySelector(".save-failure")?.textContent?.length).toBeGreaterThan(0);
    });
  });

  it("offers retry for the saved sign-in intention without toggling it away", async () => {
    const initial = { ...dump(), setup: { ...dump().setup, wizard: "done" as const,
      start_at_login: true, login_apply_failed: true } };
    app.__test__.setRoute("sources");
    invokeMock.mockImplementation(async (command) => command === "get_health"
      ? { ...initial, setup: { ...initial.setup, login_apply_failed: false } } as never : undefined as never);
    app.__test__.renderSettings(initial);
    const retry = document.querySelector<HTMLButtonElement>(".wizard-start-at-login button")!;
    expect(retry).not.toBeNull();
    retry.click();
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("retry_save"));
    await vi.waitFor(() => expect(byId(ids["settings.setup.startAtLogin.error"])).toBeNull());
    expect((byId(ids["settings.setup.startAtLogin"]) as HTMLInputElement).checked).toBe(true);
  });

  describe("wizard step numbering", () => {
    it("numbers a handoff dump 5 / 5 / 6 / 7 and gives a standalone dump no step line", () => {
      const steps = [
        ["mic", "5"],
        ["system_audio", "5"],
        ["screen", "6"],
        ["connection", "7"],
      ] as const;
      for (const extra of [{ handoff_id: "11111111-1111-4111-8111-111111111111", handoff_revision: 1 }, {}]) {
        const continuing = "handoff_id" in extra;
        for (const [wizard, number] of steps) {
          app.__test__.renderSettings({
            ...observingDump(),
            setup: {
          sequence_enabled: true,
              mic: false,
              system_audio: false,
              screen: false,
              start_at_login: false,
              wizard,
              notice_dismissed: false,
              ...extra,
            },
          });
          expect(present(ids["settings.setup.step"]).getAttribute("data-step")).toBe(wizard);
          if (continuing) {
            const stepNum = present(ids["settings.setup.stepNumber"]);
            expect(stepNum.getAttribute("data-step-number")).toBe(number);
          } else {
            expect(byId(ids["settings.setup.stepNumber"])).toBeNull();
          }
        }
      }
    });
  });

  describe("setup continuation on the home route", () => {
    const continuation = (setup: Record<string, unknown>, pairing: Record<string, unknown> = {}) => ({
      ...observingDump(),
      sync: {
        ...observingDump().sync,
        pairing: {
          phase: "not_paired" as const,
          journal_label: null,
          detail: null,
          ...pairing,
        },
      },
      setup: {
          sequence_enabled: true,
        mic: false,
        system_audio: true,
        screen: false,
        start_at_login: false,
        wizard: "done" as const,
        notice_dismissed: true,
        ...setup,
      },
    });

    it("fills an empty field from accepted progress on the default home route", async () => {
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? ("https://go.solstone.app/p#accepted" as never) : (undefined as never),
      );
      const initial = continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 10,
        handoff_expired: false,
      });
      app.__test__.renderSettings(initial);
      await vi.waitFor(() => {
        expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(
          "https://go.solstone.app/p#accepted",
        );
      });
      expect(initial.setup.wizard).toBe("done");
      expect(initial.setup.mic).toBe(false);
      expect(initial.setup.system_audio).toBe(true);
    });

    it.each(["empty", "failure"])("never restores a capability from health after %s private prefill", async (mode) => {
      const secret = "https://go.solstone.app/p#health-capability-canary";
      invokeMock.mockImplementation(async (command) => {
        if (command === "pairing_prefill") {
          if (mode === "failure") throw new Error("prefill unavailable");
          return "" as never;
        }
        return undefined as never;
      });
      app.__test__.renderSettings(continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 10,
        handoff_link: secret,
        deferred_link: secret,
      }));
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("pairing_prefill"));
      await Promise.resolve();
      await Promise.resolve();
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("");
      expect(document.body.textContent).not.toContain(secret);
      expect(byId(ids["settings.setup.deferredUse"])).toBeNull();
    });

    it("fills an empty field when a later prefill returns a link", async () => {
      let link = "";
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? (link as never) : (undefined as never),
      );
      const initial = continuation({ handoff_deferred: true });
      app.__test__.renderSettings(initial);
      await vi.waitFor(() => expect(byId(ids["settings.pairing.input"])).not.toBeNull());
      expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("");
      link = "https://go.solstone.app/p#later";
      app.__test__.renderSettings(initial);
      await vi.waitFor(() => {
        expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(link);
      });
    });

    it("keeps typed text when a later prefill arrives", async () => {
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? ("https://go.solstone.app/p#later" as never) : (undefined as never),
      );
      const initial = continuation({ handoff_deferred: true });
      app.__test__.renderSettings(initial);
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = "typed-by-owner";
      input.dispatchEvent(new Event("input"));
      app.__test__.renderSettings(initial);
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("typed-by-owner");
    });

    it("empties the field once pairing has used its link", async () => {
      const link = "https://go.solstone.app/p#used";
      app.__test__.setRoute("journal");
      app.__test__.renderSettings(continuation({}));
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = link;
      input.dispatchEvent(new Event("input"));
      present(ids["settings.pairing.submit"]).click();
      await vi.waitFor(() => expect(input.value).toBe(""));
      expect(invokeMock).toHaveBeenCalledWith("pair", { link });

      app.__test__.renderSettings(continuation({}, { phase: "paired", journal_label: "journal", binding: "bound" }));
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("");
    });

    it("keeps a link that replaced the used one while pairing ran, and keeps a link that failed", async () => {
      let release: () => void = () => {};
      invokeMock.mockImplementation((command) =>
        command === "pair"
          ? (new Promise<void>((resolve) => {
              release = resolve;
            }) as never)
          : (Promise.resolve(undefined) as never),
      );
      app.__test__.setRoute("journal");
      app.__test__.renderSettings(continuation({}));
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = "https://go.solstone.app/p#used";
      input.dispatchEvent(new Event("input"));
      present(ids["settings.pairing.submit"]).click();
      input.value = "https://go.solstone.app/p#waiting";
      input.dispatchEvent(new Event("input"));
      release();
      await Promise.resolve();
      await Promise.resolve();
      app.__test__.renderSettings(continuation({}, { phase: "paired", journal_label: "journal", binding: "bound" }));
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(
        "https://go.solstone.app/p#waiting",
      );

      resetRoot();
      invokeMock.mockImplementation(async (command) => {
        if (command === "pair") throw new Error("pairing failed");
        return undefined as never;
      });
      app.__test__.setRoute("journal");
      app.__test__.renderSettings(continuation({}));
      const failed = present(ids["settings.pairing.input"]) as HTMLInputElement;
      failed.value = "https://go.solstone.app/p#failed";
      failed.dispatchEvent(new Event("input"));
      present(ids["settings.pairing.submit"]).click();
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("pair", { link: "https://go.solstone.app/p#failed" }));
      await Promise.resolve();
      app.__test__.renderSettings(continuation({}, { phase: "failed" }));
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(
        "https://go.solstone.app/p#failed",
      );
    });

    it("reads a link that arrived while the window was open once it comes forward", async () => {
      let link = "";
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? (link as never) : (undefined as never),
      );
      app.__test__.renderSettings(continuation({ wizard: "connection" as const }));
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("pairing_prefill"));
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("");
      link = "https://go.solstone.app/p#arrived";
      app.__test__.refreshPairingPrefillOnFocus();
      await vi.waitFor(() => {
        expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(link);
      });
    });

    it("coming forward leaves typed text and a paired journal as they are", async () => {
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? ("https://go.solstone.app/p#arrived" as never) : (undefined as never),
      );
      const prefillCalls = () => invokeMock.mock.calls.filter((call) => call[0] === "pairing_prefill").length;
      app.__test__.renderSettings(continuation({ wizard: "connection" as const }));
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = "typed-by-owner";
      input.dispatchEvent(new Event("input"));
      const typedCalls = prefillCalls();
      app.__test__.refreshPairingPrefillOnFocus();
      await Promise.resolve();
      expect(prefillCalls()).toBe(typedCalls);
      expect((present(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe("typed-by-owner");

      app.__test__.reset();
      app.__test__.renderSettings(continuation({}, { phase: "paired", journal_label: "incumbent", binding: "bound" }));
      const pairedCalls = prefillCalls();
      app.__test__.refreshPairingPrefillOnFocus();
      await Promise.resolve();
      expect(prefillCalls()).toBe(pairedCalls);
    });

    it("opens the journal pane when a pairing link waits on a finished setup", async () => {
      const link = "https://go.solstone.app/p#waiting";
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? (link as never) : (undefined as never),
      );
      app.__test__.renderSettings(continuation({}));
      await vi.waitFor(() => {
        expect(app.__test__.getRoute()).toBe("journal");
        expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(link);
      });
    });

    it("stays where it is with no waiting link, another pane, a paired journal or setup in progress", async () => {
      let link = "";
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? (link as never) : (undefined as never),
      );
      const settle = async () => {
        for (let i = 0; i < 5; i++) await Promise.resolve();
      };
      app.__test__.renderSettings(continuation({}));
      await settle();
      expect(app.__test__.getRoute()).toBe("home");

      link = "https://go.solstone.app/p#waiting";
      const cases: Array<() => void> = [
        () => {
          app.__test__.setRoute("sources");
          app.__test__.renderSettings(continuation({}));
        },
        () => app.__test__.renderSettings(continuation({}, { phase: "paired", journal_label: "incumbent", binding: "bound" })),
        () => app.__test__.renderSettings(continuation({ wizard: "connection" as const })),
      ];
      for (const [index, render] of cases.entries()) {
        app.__test__.reset();
        render();
        await settle();
        expect(app.__test__.getRoute()).toBe(index === 0 ? "sources" : "home");
      }
    });

    it("shows native connect on a finished wizard without changing sources", () => {
      const initial = continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 4,
        handoff_expired: false,
      });
      app.__test__.renderSettings(initial);
      expect(present(ids["settings.setup.nativeConnect"]).tagName).toBe("BUTTON");
      expect(initial.setup.wizard).toBe("done");
      expect(initial.setup.mic).toBe(false);
      expect(initial.setup.system_audio).toBe(true);
      expect(initial.setup.screen).toBe(false);
      expect(invokeMock).not.toHaveBeenCalledWith("set_wizard_step", expect.anything());
      expect(invokeMock).not.toHaveBeenCalledWith("set_source_enabled", expect.anything());
    });

    it("shows retry during an active ceremony and hides native connect", () => {
      app.__test__.renderSettings(continuation({ handoff_deferred: true }));
      expect(present(ids["settings.setup.linkRetry"])).not.toBeNull();
      expect(byId(ids["settings.setup.nativeConnect"])).toBeNull();
    });

    it("dismisses a rejection marker when no deferred link remains", async () => {
      app.__test__.renderSettings(continuation({ handoff_rejected: true }));
      expect(byId(ids["settings.setup.deferredUse"])).toBeNull();
      present(ids["settings.setup.deferredDismiss"]).click();
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("dismiss_deferred_link"));
      expect(invokeMock).not.toHaveBeenCalledWith("pair", expect.anything());
    });

    it.each([
      ["settings.setup.deferredUse", "use_deferred_link"],
      ["settings.setup.deferredDismiss", "dismiss_deferred_link"],
    ] as const)("shows an ordinary deferred action failure beside %s and blocks duplicate actions", async (id, command) => {
      const initial = continuation({ has_deferred_link: true });
      let reject: (reason: unknown) => void = () => {};
      const pending = new Promise((_resolve, fail) => { reject = fail; });
      invokeMock.mockImplementation((called) => {
        if (called === command) return pending;
        if (called === "get_health") return Promise.resolve(initial as never);
        return Promise.resolve(undefined as never);
      });
      app.__test__.renderSettings(initial);
      const button = present(ids[id]);
      button.click(); button.click();
      const other = id === "settings.setup.deferredUse" ? "settings.setup.deferredDismiss" : "settings.setup.deferredUse";
      present(ids[other]).click();
      expect(invokeMock.mock.calls.filter(([called]) => called === command)).toHaveLength(1);
      reject(new Error("link could not be saved"));
      await vi.waitFor(() => expect(document.querySelector('[data-setup-error="deferred"]')?.textContent).toBe("link could not be saved"));
      expect(present(ids[id])).not.toBeNull();
      expect(byId(ids["settings.setup.linkRetry"])).toBeNull();
    });

    it("explicitly selects a deferred link over typed text without pairing", async () => {
      invokeMock.mockImplementation(async (command) => {
        if (command === "use_deferred_link") return "https://go.solstone.app/p#deferred" as never;
        if (command === "pairing_prefill") return "" as never;
        return undefined as never;
      });
      app.__test__.renderSettings(continuation({ has_deferred_link: true }));
      const typed = present(ids["settings.pairing.input"]) as HTMLInputElement;
      typed.value = "owner-typed-draft";
      typed.dispatchEvent(new Event("input"));
      present(ids["settings.setup.deferredUse"]).click();
      await vi.waitFor(() => {
        expect((byId(ids["settings.pairing.input"]) as HTMLInputElement).value).toBe(
          "https://go.solstone.app/p#deferred",
        );
      });
      expect(invokeMock).toHaveBeenCalledWith("use_deferred_link");
      expect(invokeMock).not.toHaveBeenCalledWith("pair", expect.anything());

      app.__test__.reset();
      invokeMock.mockClear();
      app.__test__.renderSettings(continuation({ has_deferred_link: true }));
      present(ids["settings.setup.deferredDismiss"]).click();
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("dismiss_deferred_link"));
      expect(invokeMock).not.toHaveBeenCalledWith("pair", expect.anything());
    });

    it("explains an arriving link before offering to use or dismiss it", () => {
      app.__test__.renderSettings(continuation({ has_deferred_link: true }));
      const row = present(ids["settings.setup.deferredUse"]).parentElement as HTMLElement;
      const note = row.previousElementSibling as HTMLElement | null;
      expect(note?.tagName).toBe("P");
      expect(note?.textContent?.trim().length ?? 0).toBeGreaterThan(0);

      app.__test__.reset();
      app.__test__.renderSettings(continuation({ has_deferred_link: false }));
      expect(document.querySelector(".setup-deferred-note")).toBeNull();
    });

    it("continues from the accepted handoff even when the pair field is dirty", async () => {
      const initial = continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 9,
        handoff_expired: false,
      });
      invokeMock.mockImplementation(async (command) =>
        command === "pairing_prefill" ? ("" as never) : (undefined as never),
      );
      app.__test__.renderSettings(initial);
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = "https://go.solstone.app/p#typed";
      input.dispatchEvent(new Event("input"));
      present(ids["settings.setup.nativeConnect"]).click();
      await vi.waitFor(() =>
        expect(invokeMock).toHaveBeenCalledWith("connect_native_handoff", {
          handoffId: "11111111-1111-4111-8111-111111111111",
          revision: 9,
        }),
      );
      expect(invokeMock.mock.calls.map((call) => call[0])).not.toContain("pair");
    });

    it("offers a fresh handoff beside the incumbent and only connects on owner action", async () => {
      const initial = continuation({
        handoff_id: "22222222-2222-4222-8222-222222222222",
        handoff_revision: 12,
        handoff_expired: false,
        handoff_new_connection: true,
        wizard: "connection" as const,
      }, { phase: "paired", journal_label: "incumbent", binding: "old-binding",
        mark: sampleMarkSpec("liquefy", "smock", "#3b82f6") });
      app.__test__.renderSettings(initial);
      expect(byId(ids["settings.setup.next"])).toBeNull();
      expect(present(ids["settings.pairing.mark"]).textContent).toContain("liquefy · smock");
      expect(invokeMock).not.toHaveBeenCalledWith("connect_native_handoff", expect.anything());
      present(ids["settings.setup.nativeConnect"]).click();
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("connect_native_handoff", {
        handoffId: "22222222-2222-4222-8222-222222222222", revision: 12,
      }));
      expect(initial.sync.pairing.binding).toBe("old-binding");
    });

    it.each([
      { handoff_new_connection: false },
      {},
      { handoff_new_connection: true, handoff_expired: true },
      { handoff_new_connection: false, handoff_persist_failed: true },
    ])("does not replay an incumbent or offer an unavailable paired handoff: %j", (extra) => {
      app.__test__.renderSettings(continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 3,
        handoff_expired: false,
        ...extra,
      }, { phase: "paired", journal_label: "incumbent", binding: "old-binding" }));
      expect(byId(ids["settings.setup.nativeConnect"])).toBeNull();
      expect(invokeMock).not.toHaveBeenCalledWith("connect_native_handoff", expect.anything());
    });

    it.each([
      { phase: "awaiting_confirmation", finish_connecting: true },
      { phase: "paired", setup_browser_ready: true },
    ])("keeps the established action ahead of a newer handoff: %j", (pairing) => {
      app.__test__.renderSettings(continuation({
        handoff_id: "22222222-2222-4222-8222-222222222222",
        handoff_revision: 12,
        handoff_new_connection: true,
        handoff_expired: false,
        wizard: "connection" as const,
      }, { ...pairing, binding: "old-binding" }));
      expect(byId(ids["settings.setup.nativeConnect"])).toBeNull();
      expect(byId(ids["settings.setup.next"])).toBeNull();
      expect(invokeMock).not.toHaveBeenCalledWith("connect_native_handoff", expect.anything());
    });

    it("leaves opening the journal as the connect step's only action once paired", () => {
      app.__test__.renderSettings(continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 3,
        handoff_expired: false,
        handoff_new_connection: false,
        wizard: "connection" as const,
      }, { phase: "paired", setup_browser_ready: true, binding: "bound" }));
      present(ids["settings.setup.browserJournal"]);
      expect(byId(ids["settings.setup.freshLink"])).toBeNull();
      expect(byId(ids["settings.pairing.input"])).toBeNull();
      expect(byId(ids["settings.pairing.submit"])).toBeNull();

      app.__test__.reset();
      app.__test__.renderSettings(continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 3,
        handoff_expired: false,
        wizard: "connection" as const,
      }, { phase: "not_paired" }));
      present(ids["settings.setup.freshLink"]);
      present(ids["settings.pairing.input"]);
      present(ids["settings.pairing.submit"]);
    });

    it("shows a continuation failure and does not invoke twice while pending", async () => {
      const initial = continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 9,
        handoff_expired: false,
      });
      let calls = 0;
      invokeMock.mockImplementation(async (command) => {
        if (command === "connect_native_handoff") {
          calls += 1;
          throw new Error("connect failed");
        }
        if (command === "get_health") return initial as never;
        return undefined as never;
      });
      app.__test__.renderSettings(initial);
      present(ids["settings.setup.nativeConnect"]).click();
      await vi.waitFor(() => expect(document.querySelector('[data-setup-error="connect"]')).not.toBeNull());
      expect(calls).toBe(1);
      present(ids["settings.setup.nativeConnect"]).click();
      await vi.waitFor(() => expect(calls).toBe(2));

      app.__test__.reset();
      calls = 0;
      let release: (value: unknown) => void = () => {};
      const pending = new Promise((resolve) => {
        release = resolve;
      });
      invokeMock.mockImplementation((command) => {
        if (command === "connect_native_handoff") {
          calls += 1;
          return pending;
        }
        return Promise.resolve(initial as never);
      });
      app.__test__.renderSettings(initial);
      const button = present(ids["settings.setup.nativeConnect"]);
      button.click();
      button.click();
      expect(calls).toBe(1);
      release(undefined);
      await pending;
    });

    it("disables an expired native button and still pairs an ordinary link", () => {
      app.__test__.renderSettings(continuation({
        handoff_id: "11111111-1111-4111-8111-111111111111",
        handoff_revision: 1,
        handoff_expired: true,
      }));
      const native = present(ids["settings.setup.nativeConnect"]) as HTMLButtonElement;
      expect(native.disabled).toBe(true);
      const freshLink = present(ids["settings.setup.freshLink"]);
      expect(freshLink.tagName).not.toBe("BUTTON");
      const input = present(ids["settings.pairing.input"]) as HTMLInputElement;
      input.value = "https://go.solstone.app/p#ordinary";
      input.dispatchEvent(new Event("input"));
      present(ids["settings.pairing.submit"]).click();
      expect(invokeMock).toHaveBeenCalledWith("pair", { link: "https://go.solstone.app/p#ordinary" });
    });

    it("shows a finish failure and allows one retry after it settles", async () => {
      const initial = continuation(
        {},
        { phase: "awaiting_confirmation", finish_connecting: true, binding: "abc" },
      );
      let calls = 0;
      invokeMock.mockImplementation(async (command) => {
        if (command === "finish_connecting") {
          calls += 1;
          throw new Error("finish failed");
        }
        if (command === "get_health") return initial as never;
        return undefined as never;
      });
      app.__test__.renderSettings(initial);
      const button = present(ids["settings.pairing.finishConnecting"]);
      button.click();
      await vi.waitFor(() => expect(document.querySelector('[data-setup-error="finish"]')).not.toBeNull());
      expect(calls).toBe(1);
      present(ids["settings.pairing.finishConnecting"]).click();
      await vi.waitFor(() => expect(calls).toBe(2));
    });

    it("does not invoke finish twice while the first call is pending", async () => {
      const initial = continuation(
        {},
        { phase: "awaiting_confirmation", finish_connecting: true, binding: "abc" },
      );
      let calls = 0;
      let release: (value: unknown) => void = () => {};
      const pending = new Promise((resolve) => {
        release = resolve;
      });
      invokeMock.mockImplementation((command) => {
        if (command === "finish_connecting") {
          calls += 1;
          return pending;
        }
        return Promise.resolve(initial as never);
      });
      app.__test__.renderSettings(initial);
      const button = present(ids["settings.pairing.finishConnecting"]);
      button.click();
      button.click();
      expect(calls).toBe(1);
      release(undefined);
      await pending;
    });

    it("shows a browser failure beside the open control", async () => {
      const initial = continuation(
        {},
        { phase: "paired", setup_browser_ready: true, journal_label: "journal", binding: "abc" },
      );
      invokeMock.mockImplementation(async (command) => {
        if (command === "open_setup_journal") throw new Error("browser failed");
        if (command === "get_health") return initial as never;
        return undefined as never;
      });
      app.__test__.renderSettings(initial);
      present(ids["settings.setup.browserJournal"]).click();
      await vi.waitFor(() => expect(document.querySelector('[data-setup-error="browser"]')).not.toBeNull());
    });

    it("asks the native final action to open and persist completion for the displayed binding", async () => {
      const initial = continuation(
        {},
        { phase: "paired", setup_browser_ready: true, journal_label: "journal", binding: "abc" },
      );
      const order: string[] = [];
      invokeMock.mockImplementation(async (command) => {
        order.push(String(command));
        if (command === "get_health") return initial as never;
        return undefined as never;
      });
      app.__test__.renderSettings(initial);
      present(ids["settings.setup.browserJournal"]).click();
      await vi.waitFor(() => expect(order).toContain("open_setup_journal"));
      expect(order).not.toContain("set_wizard_step");
      expect(order.filter((command) => command === "open_setup_journal")).toHaveLength(1);
      expect(invokeMock).toHaveBeenCalledWith("open_setup_journal", { binding: "abc" });
    });

    it("keeps the established final action after link expiry without a dead next button", async () => {
      app.__test__.renderSettings({
        ...observingDump(),
        sync: {
          ...observingDump().sync,
          pairing: {
            phase: "paired",
            journal_label: "journal",
            detail: null,
            setup_browser_ready: true,
          },
        },
        setup: {
          sequence_enabled: true,
          mic: false,
          system_audio: false,
          screen: false,
          start_at_login: false,
          wizard: "connection",
          notice_dismissed: false,
          handoff_id: "11111111-1111-4111-8111-111111111111",
          handoff_revision: 3,
          handoff_expired: true,
        },
      });
      expect(byId(ids["settings.setup.next"])).toBeNull();
      expect(byId(ids["settings.setup.nativeConnect"])).toBeNull();
      expect(present(ids["settings.setup.browserJournal"])).not.toBeNull();
      await Promise.resolve();
      expect(invokeMock).not.toHaveBeenCalledWith("set_wizard_step", { step: "done" });
    });

    it("still completes a standalone all-off skip", async () => {
      app.__test__.renderSettings({
        ...observingDump(),
        sync: {
          ...observingDump().sync,
          pairing: { phase: "not_paired", journal_label: null, detail: null },
        },
        setup: {
          sequence_enabled: true,
          mic: false,
          system_audio: false,
          screen: false,
          start_at_login: false,
          wizard: "connection",
          notice_dismissed: false,
        },
      });
      present(ids["settings.setup.notNow"]).click();
      await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("set_wizard_step", { step: "done" }));
    });
  });
});

describe("dark release composition", () => {
  beforeEach(() => { resetRoot(); invokeMock.mockReset(); invokeMock.mockResolvedValue(undefined as never); });
  it("keeps legacy Home and read-only Sources despite retained setup progress", () => {
    const dump = { ...observingDump(), setup: { sequence_enabled: false, mic: false, screen: false, system_audio: false, start_at_login: false, wizard: "mic" as const, notice_dismissed: false } };
    app.__test__.setRoute("home");
    app.__test__.renderSettings(dump);
    expect(byId(ids["settings.setup.step"])).toBeNull();
    expect(byId(ids["settings.home.sourcesNotice"])).toBeNull();
    expect(document.querySelector(".settings-home")).not.toBeNull();
    app.__test__.setRoute("sources");
    app.__test__.renderSettings(dump);
    expect(byId(ids["settings.sources.screen.state"])).not.toBeNull();
    expect(byId(ids["settings.sources.screen.enabled"])).toBeNull();
    expect(byId(ids["settings.setup.startAtLogin"])).toBeNull();
    expect(invokeMock.mock.calls.some(([name]) => name === "pairing_prefill")).toBe(false);
  });
});
