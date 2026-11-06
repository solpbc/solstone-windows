// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";

import { automationContract } from "../src/lib/contract";
import * as app from "../src/main";
import { notPairedDump, pairedWithMarkDump, sampleMarkSpec } from "./fixtures";

const ids = automationContract.automation_ids;
const invokeMock = vi.mocked(invoke);
const found = { instance_id: "journal-instance", mark: sampleMarkSpec("piano", "key") };

const byId = (id: string): HTMLElement | null =>
  document.querySelector(`[data-automation-id="${id}"]`);

function setRoute(route: "home" | "journal"): void {
  document.body.replaceChildren();
  const rootEl = document.createElement("div");
  rootEl.id = "app";
  document.body.append(rootEl);
  app.__test__.reset();
  app.__test__.setRoot(rootEl);
  app.__test__.setRoute(route);
}

function setupDump(wizard: "mic" | "connection") {
  const base = notPairedDump();
  return {
    ...base,
    setup: { ...(base as { setup?: object }).setup, sequence_enabled: true, wizard },
  };
}

async function look(): Promise<void> {
  app.__test__.lookForFoundJournal();
  await vi.waitFor(() => expect(app.__test__.foundJournalPending()).toBe(false));
}

function looks(): number {
  return invokeMock.mock.calls.filter(([command]) => command === "find_local_journal").length;
}

function answering(journal: typeof found | null, connect?: () => Promise<void>): void {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "find_local_journal") return journal;
    if (command === "connect_found_journal") return connect ? connect() : undefined;
    if (command === "get_health") return app.__test__.getHealth();
    return undefined;
  });
}

beforeEach(() => {
  invokeMock.mockReset();
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
});

describe("a journal found on this computer", () => {
  it("is offered on the journal pane with its mark, one button, and the paste field beside it", async () => {
    setRoute("journal");
    answering(found);
    app.__test__.renderSettings(notPairedDump());
    expect(byId(ids["settings.pairing.foundJournal"])).toBeNull();

    await look();

    const offer = byId(ids["settings.pairing.foundJournal"]);
    expect(offer).not.toBeNull();
    expect(offer?.querySelector(".mark-card")?.textContent?.toLowerCase()).toContain("piano");
    expect(byId(ids["settings.pairing.foundConnect"])).not.toBeNull();
    expect(byId(ids["settings.pairing.input"])).not.toBeNull();
    expect(byId(ids["settings.pairing.submit"])).not.toBeNull();
    // Only the found journal's mark is on screen, not the generic one too.
    expect(byId(ids["settings.pairing.mark"])).toBeNull();
  });

  it("is offered at the connect step of setup, and not at the steps before it", async () => {
    setRoute("home");
    answering(found);
    app.__test__.renderSettings(setupDump("mic"));
    await look();
    expect(looks()).toBe(0);

    setRoute("home");
    app.__test__.renderSettings(setupDump("connection"));
    await look();
    expect(looks()).toBe(1);
    expect(byId(ids["settings.pairing.foundJournal"])).not.toBeNull();
  });

  it("appears once a journal starts answering, and goes when it stops", async () => {
    setRoute("journal");
    answering(null);
    app.__test__.renderSettings(notPairedDump());
    await look();
    expect(byId(ids["settings.pairing.foundJournal"])).toBeNull();

    vi.useFakeTimers({ now: Date.now(), toFake: ["Date"] });
    try {
      vi.setSystemTime(Date.now() + 4000);
      answering(found);
      await look();
      expect(byId(ids["settings.pairing.foundJournal"])).not.toBeNull();

      vi.setSystemTime(Date.now() + 4000);
      answering(null);
      await look();
      expect(byId(ids["settings.pairing.foundJournal"])).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it.each([
    ["the app holds a pairing", () => pairedWithMarkDump(sampleMarkSpec()), "journal"],
    [
      "a pairing is waiting for its mark answer",
      () => {
        const base = pairedWithMarkDump(sampleMarkSpec());
        return {
          ...base,
          sync: {
            ...base.sync,
            pairing: { ...base.sync.pairing, phase: "awaiting_confirmation" as const, binding: "b" },
          },
        };
      },
      "journal",
    ],
    ["another pane is showing", () => notPairedDump(), "home"],
  ] as const)("is not looked for while %s", async (_name, dump, route) => {
    setRoute(route);
    answering(found);
    app.__test__.renderSettings(dump() as ReturnType<typeof notPairedDump>);
    await look();
    expect(looks()).toBe(0);
    expect(byId(ids["settings.pairing.foundJournal"])).toBeNull();
  });

  it("is not looked for while the window is hidden", async () => {
    setRoute("journal");
    answering(found);
    app.__test__.renderSettings(notPairedDump());
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    await look();
    expect(looks()).toBe(0);
  });

  it("is not looked for while a setup handoff is waiting to be used", async () => {
    setRoute("journal");
    answering(found);
    const dump = setupDump("connection");
    app.__test__.renderSettings({
      ...dump,
      setup: { ...dump.setup, wizard: "done", handoff_id: "h", handoff_expired: false },
    } as ReturnType<typeof notPairedDump>);
    await look();
    expect(looks()).toBe(0);
  });

  it("connects only on the press, naming the journal it offered", async () => {
    setRoute("journal");
    const connect = vi.fn(async () => {
      app.__test__.setHealth(pairedWithMarkDump(found.mark) as ReturnType<typeof notPairedDump>);
    });
    answering(found, connect);
    app.__test__.renderSettings(notPairedDump());
    await look();
    expect(connect).not.toHaveBeenCalled();

    byId(ids["settings.pairing.foundConnect"])?.click();
    await vi.waitFor(() => expect(connect).toHaveBeenCalledTimes(1));
    expect(invokeMock).toHaveBeenCalledWith("connect_found_journal", {
      instanceId: found.instance_id,
    });
    await vi.waitFor(() =>
      expect(byId(ids["settings.pairing.state"])?.textContent).toContain("piano"),
    );
    expect(byId(ids["settings.pairing.foundJournal"])).toBeNull();
    expect(byId(ids["settings.pairing.confirmYes"])).toBeNull();
  });

  it.each([
    ["connected", true],
    ["connecting", false],
    ["offline", false],
  ] as const)("after the press, a journal link that is %s decides whether the status says connected", async (link, says) => {
    setRoute("journal");
    const paired = pairedWithMarkDump(found.mark);
    const withLink = (journal_link: "connected" | "connecting" | "offline") =>
      ({ ...paired, sync: { ...paired.sync, journal_link } }) as ReturnType<typeof notPairedDump>;
    answering(found, async () => {
      app.__test__.setHealth(withLink("connected"));
    });
    app.__test__.renderSettings(notPairedDump());
    await look();
    byId(ids["settings.pairing.foundConnect"])?.click();
    await vi.waitFor(() =>
      expect(byId(ids["settings.pairing.state"])?.textContent).toContain("connected to"),
    );

    app.__test__.renderSettings(withLink(link));
    const status = byId(ids["settings.pairing.state"])?.textContent ?? "";
    expect(status).toContain("piano");
    expect(status.includes("connected to")).toBe(says);
    expect(status.includes("paired with")).toBe(!says);

    // A drop and a recovery are both earned from the link, with no new press.
    app.__test__.renderSettings(withLink("connected"));
    expect(byId(ids["settings.pairing.state"])?.textContent).toContain("connected to");
  });

  it("keeps the offer on screen when a look from before the press comes back empty", async () => {
    setRoute("journal");
    answering(found);
    app.__test__.renderSettings(notPairedDump());
    await look();
    let answerLook: (value: null) => void = () => {};
    let finishConnect: () => void = () => {};
    invokeMock.mockImplementation((command: string) => {
      if (command === "find_local_journal") {
        return new Promise((resolve) => {
          answerLook = resolve as (value: null) => void;
        });
      }
      if (command === "connect_found_journal") {
        return new Promise<void>((resolve) => {
          finishConnect = resolve;
        });
      }
      return Promise.resolve(undefined);
    });
    vi.useFakeTimers({ now: Date.now(), toFake: ["Date"] });
    try {
      vi.setSystemTime(Date.now() + 4000);
      app.__test__.lookForFoundJournal();
    } finally {
      vi.useRealTimers();
    }
    byId(ids["settings.pairing.foundConnect"])?.click();
    await vi.waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("connect_found_journal", {
        instanceId: found.instance_id,
      }),
    );
    answerLook(null);
    await vi.waitFor(() => expect(app.__test__.foundJournalPending()).toBe(false));
    expect(byId(ids["settings.pairing.foundJournal"])).not.toBeNull();
    finishConnect();
  });

  it("says a failed verification connected nothing, and keeps the offer and the paste field", async () => {
    setRoute("journal");
    answering(found, async () => {
      const base = notPairedDump();
      app.__test__.setHealth({
        ...base,
        sync: {
          ...base.sync,
          pairing: { ...base.sync.pairing, detail: "same_computer_unverified" },
        },
      });
      throw new Error("unverified");
    });
    app.__test__.renderSettings(notPairedDump());
    await look();
    byId(ids["settings.pairing.foundConnect"])?.click();
    await vi.waitFor(() => expect(byId(ids["settings.pairing.foundFailed"])).not.toBeNull());
    expect(byId(ids["settings.pairing.input"])).not.toBeNull();
    expect(byId(ids["settings.pairing.foundConnect"])).not.toBeNull();
    expect(byId(ids["settings.pairing.confirmYes"])).toBeNull();
  });
});
