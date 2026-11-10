import { describe, expect, it } from "vitest";
import {
  addMoreSources,
  audioTile,
  browsersTile,
  counterFacts,
  deckColumns,
  deckTiles,
  derivedComputerSoundWanted,
  greeting,
  headerWhilePaused,
  journalPill,
  legacyRoute,
  settingsNavigate,
  menuRows,
  obsidianTile,
  pauseTimeLeft,
  restartPlace,
  screenTile,
  shelfBadges,
  spaceOnTile,
  statusRowOpens,
  widthClass,
  type AudioTileInput,
  type CounterDocument,
  type JournalPillInput,
  type ScreenTileInput,
} from "../src/shell/derive";

const quietAudio: AudioTileInput = {
  micEnabled: false,
  micAccessDenied: false,
  micDevicePresent: true,
  micTakingIn: false,
  systemAudioEnabled: false,
  systemAudioFaulted: false,
  systemAudioTakingIn: false,
  appPaused: false,
  wanted: null,
};

const quietScreen: ScreenTileInput = {
  enabled: false,
  accessDenied: false,
  failed: false,
  activityFailed: false,
  activityOn: false,
  appPaused: false,
  running: false,
  permissionAsked: true,
};

const caughtUp: JournalPillInput = {
  paired: true,
  needsNewLink: false,
  refused: false,
  syncPaused: false,
  held: false,
  offline: false,
  connecting: false,
  waitingToSync: false,
  waiting: 0,
  sourceNeedsAttention: false,
};

describe("audio", () => {
  it("keeps a denied microphone as needs attention whether computer sound is on or off", () => {
    for (const systemAudioEnabled of [true, false]) {
      const tile = audioTile({
        ...quietAudio,
        micEnabled: true,
        micAccessDenied: true,
        systemAudioEnabled,
        systemAudioTakingIn: systemAudioEnabled,
      });
      expect(tile.word).toBe("needs attention");
    }
  });

  it("is on with no microphone when computer sound is on, and off when computer sound is off", () => {
    expect(
      audioTile({
        ...quietAudio,
        micDevicePresent: false,
        systemAudioEnabled: true,
        systemAudioTakingIn: true,
      }).word,
    ).toBe("on");
    expect(audioTile({ ...quietAudio, micDevicePresent: false }).word).toBe("off");
  });

  it("derives a missing computer-sound bit from the live keys and does not treat both-off as false", () => {
    expect(derivedComputerSoundWanted(true, false, null)).toBe(false);
    expect(derivedComputerSoundWanted(false, true, null)).toBe(true);
    expect(derivedComputerSoundWanted(false, false, null)).toBe(true);
    expect(derivedComputerSoundWanted(true, true, null)).toBe(true);
    expect(derivedComputerSoundWanted(false, true, false)).toBe(false);
  });

  it("shows the tile on when only computer sound is taking in", () => {
    const tile = audioTile({
      ...quietAudio,
      systemAudioEnabled: true,
      systemAudioTakingIn: true,
      wanted: null,
    });
    expect(tile.word).toBe("on");
    expect(tile.switchOn).toBe(true);
  });

  it("does not let off hide a paused tile that is still on", () => {
    expect(audioTile({ ...quietAudio, appPaused: true }).word).toBe("off");
    expect(
      audioTile({ ...quietAudio, micEnabled: true, micTakingIn: true, appPaused: true }).word,
    ).toBe("paused");
  });

  it("names a microphone only while audio is on and that microphone is taking something in", () => {
    expect(
      audioTile({ ...quietAudio, micEnabled: true, micDevicePresent: true, micTakingIn: true }).namesMicrophone,
    ).toBe(true);
    expect(
      audioTile({
        ...quietAudio,
        micDevicePresent: false,
        systemAudioEnabled: true,
        systemAudioTakingIn: true,
      }).namesMicrophone,
    ).toBe(false);
    expect(
      audioTile({ ...quietAudio, micEnabled: true, micAccessDenied: true, micTakingIn: true }).namesMicrophone,
    ).toBe(false);
    expect(
      audioTile({
        ...quietAudio,
        micEnabled: false,
        micTakingIn: true,
        systemAudioEnabled: true,
        systemAudioTakingIn: true,
      }).namesMicrophone,
    ).toBe(false);
  });
});

describe("screen", () => {
  it("stays off when activity is kept on, and carries that fact", () => {
    const tile = screenTile({ ...quietScreen, activityOn: true });
    expect(tile.word).toBe("off");
    expect(tile.activityKept).toBe(true);
    expect(tile.activityAlongside).toBe(false);
  });

  it("names activity beside an on tile and not beside a fault", () => {
    const on = screenTile({ ...quietScreen, enabled: true, running: true, activityOn: true });
    expect(on.word).toBe("on");
    expect(on.activityAlongside).toBe(true);
    expect(on.activityKept).toBe(false);
    const faulted = screenTile({
      ...quietScreen,
      enabled: true,
      running: true,
      activityOn: true,
      accessDenied: true,
    });
    expect(faulted.word).toBe("needs attention");
    expect(faulted.activityAlongside).toBe(false);
  });

  it("stays paused when activity is still going, and carries that fact", () => {
    const tile = screenTile({ ...quietScreen, enabled: true, appPaused: true, activityOn: true });
    expect(tile.word).toBe("paused");
    expect(tile.activityKept).toBe(true);
  });

  it("is needs attention when access is denied", () => {
    expect(screenTile({ ...quietScreen, enabled: true, accessDenied: true }).word).toBe(
      "needs attention",
    );
  });
});

describe("browsers", () => {
  it("is on with none connected, needs attention when the store is full, and not a fault when unpaired", () => {
    expect(browsersTile({ capture: "permitted", deliveryFailed: false, appPaused: false })).toEqual({
      word: "on",
      switchOn: null,
      activityKept: false,
      activityAlongside: false,
      namesMicrophone: false,
    });
    expect(browsersTile({ capture: "intake_off", deliveryFailed: false, appPaused: false }).word).toBe(
      "needs attention",
    );
    expect(browsersTile({ capture: "not_paired", deliveryFailed: false, appPaused: false }).word).not.toBe(
      "needs attention",
    );
  });
});

describe("obsidian", () => {
  it("maps the existing state names", () => {
    expect(obsidianTile({ state: "needs_attention", vault: "v", foundCount: 1 }).word).toBe(
      "needs attention",
    );
    expect(obsidianTile({ state: "paused", vault: "v", foundCount: 1 }).word).toBe("paused");
    expect(obsidianTile({ state: "on", vault: "v", foundCount: 1 }).word).toBe("on");
    expect(obsidianTile({ state: "waiting_for_journal_update", vault: "v", foundCount: 1 }).word).toBe(
      "setting up",
    );
    expect(obsidianTile({ state: "confirm", vault: null, foundCount: 1 }).word).toBe("setting up");
    expect(obsidianTile({ state: "checking", vault: null, foundCount: 0 }).word).toBe("checking…");
    expect(obsidianTile({ state: "off", vault: null, foundCount: 0 }).word).toBe("ready to set up");
    expect(obsidianTile({ state: "off", vault: "v", foundCount: 1 }).word).toBe("off");
  });
});

describe("journal pill and pause header", () => {
  it("ignores a source fault and does not become a source state word", () => {
    const calm = journalPill(caughtUp);
    const faulted = journalPill({ ...caughtUp, sourceNeedsAttention: true });
    expect(faulted).toBe(calm);
    expect(faulted).not.toBe("needs attention");
    expect(faulted).not.toBe("on");
    expect(faulted).not.toBe("off");
  });

  it("uses a journal word for the computer-only cases", () => {
    expect(journalPill({ ...caughtUp, syncPaused: true, waiting: 2 })).toContain("sync paused");
    expect(journalPill({ ...caughtUp, needsNewLink: true })).toBe("your journal needs a new link");
    expect(journalPill({ ...caughtUp, refused: true, waiting: 0 })).toBe("pair again");
  });

  it("shows the time left and one resume while paused, and no pause durations", () => {
    expect(headerWhilePaused(true)).toEqual({ timeLeft: true, resume: true, pauseDurations: false });
    expect(headerWhilePaused(false).resume).toBe(false);
  });

  it("rounds a pause up to whole minutes and keeps indefinite apart", () => {
    expect(pauseTimeLeft(null)).toEqual({ kind: "untilResume" });
    expect(pauseTimeLeft(30)).toEqual({ kind: "lessThanAMinute" });
    expect(pauseTimeLeft(61)).toEqual({ kind: "minutes", minutes: 2 });
    expect(pauseTimeLeft(60)).toEqual({ kind: "minutes", minutes: 1 });
  });
});

describe("menu", () => {
  it("names the first source that needs attention, once", () => {
    const rows = menuRows({
      needsAttention: ["obsidian", "screen"],
      paused: false,
      paired: true,
      journalReachable: true,
    });
    const attention = rows.filter((row) => row.kind === "attention");
    expect(attention).toEqual([{ kind: "attention", source: "screen" }]);
    expect(rows.some((row) => row.kind === "status" && row.disabled)).toBe(true);
    expect(rows.some((row) => row.kind === "pause")).toBe(true);
    expect(rows.some((row) => row.kind === "resume")).toBe(false);
    expect(rows.some((row) => row.kind === "openJournal")).toBe(true);
  });

  it("shows resume instead of pause, and omits the journal when unpaired", () => {
    const rows = menuRows({
      needsAttention: [],
      paused: true,
      paired: false,
      journalReachable: false,
    });
    expect(rows.some((row) => row.kind === "attention")).toBe(false);
    expect(rows.some((row) => row.kind === "resume")).toBe(true);
    expect(rows.some((row) => row.kind === "pause")).toBe(false);
    expect(rows.some((row) => row.kind === "openJournal")).toBe(false);
  });
});

describe("restart", () => {
  it("sits on the audio page for a microphone fault, and nowhere when the phase is not error", () => {
    expect(
      restartPlace({
        phaseError: true,
        micFaulted: true,
        systemAudioFaulted: false,
        screenFaulted: false,
        activityFaulted: false,
        storageFault: false,
      }),
    ).toBe("audio");
    expect(
      restartPlace({
        phaseError: false,
        micFaulted: true,
        systemAudioFaulted: false,
        screenFaulted: false,
        activityFaulted: false,
        storageFault: true,
      }),
    ).toBe("absent");
  });

  it("puts a storage-only fault in technical details", () => {
    expect(
      restartPlace({
        phaseError: true,
        micFaulted: false,
        systemAudioFaulted: false,
        screenFaulted: false,
        activityFaulted: false,
        storageFault: true,
      }),
    ).toBe("technicalDetails");
  });

  it("puts a screen or activity fault on the screen page, and an audio fault wins", () => {
    const quiet = {
      phaseError: true,
      micFaulted: false,
      systemAudioFaulted: false,
      screenFaulted: false,
      activityFaulted: false,
      storageFault: false,
    };
    expect(restartPlace({ ...quiet, screenFaulted: true })).toBe("screen");
    expect(restartPlace({ ...quiet, activityFaulted: true })).toBe("screen");
    expect(restartPlace({ ...quiet, systemAudioFaulted: true })).toBe("audio");
    expect(
      restartPlace({
        ...quiet,
        micFaulted: true,
        screenFaulted: true,
        activityFaulted: true,
        storageFault: true,
      }),
    ).toBe("audio");
    expect(restartPlace(quiet)).toBe("absent");
  });
});

describe("routes and widths", () => {
  it("opens the deck from sources, and a source page from the attention payload", () => {
    expect(settingsNavigate("sources")).toEqual({ row: "home", pane: "status" });
    expect(settingsNavigate("home")).toEqual({ row: "home", pane: "status" });
    expect(settingsNavigate("audio")).toEqual({ row: "home", pane: "audio" });
    expect(settingsNavigate("screen")).toEqual({ row: "home", pane: "screen" });
    expect(settingsNavigate("journal")).toEqual({ row: "journal", pane: "status" });
  });

  it("maps each old route", () => {
    expect(legacyRoute("home")).toEqual({ row: "home", pane: "status" });
    expect(legacyRoute("sources")).toEqual({ row: "home", pane: "status" });
    expect(legacyRoute("privacy")).toEqual({ row: "home", pane: "screenLeftOut" });
    expect(legacyRoute("journal")).toEqual({ row: "journal", pane: "status" });
    expect(legacyRoute("shortcut")).toEqual({ row: "thisPc", pane: "status" });
    expect(legacyRoute("storage")).toEqual({ row: "thisPc", pane: "status" });
    expect(legacyRoute("updates")).toEqual({ row: "updates", pane: "status" });
    expect(legacyRoute("help")).toEqual({ row: "help", pane: "status" });
  });

  it("opens a status row onto that source", () => {
    expect(statusRowOpens("audio")).toBe("audio");
    expect(statusRowOpens("browsers")).toBe("browsers");
  });

  it("picks the width from the inner width alone", () => {
    expect(widthClass(719)).toBe("phone");
    expect(widthClass(720)).toBe("narrower");
    expect(widthClass(1099)).toBe("narrower");
    expect(widthClass(1100)).toBe("full");
    expect(deckColumns(widthClass(1100))).toBe(3);
    expect(deckColumns(widthClass(800))).toBe(2);
    expect(deckColumns(widthClass(400))).toBe(1);
  });
});

describe("counters", () => {
  it("shows no rows for a missing read, and not a zero", () => {
    expect(counterFacts(null)).toBeNull();
  });

  it("keeps each source's own last, and does not call a quarantined zero nothing yet", () => {
    const document: CounterDocument = {
      started: "2026-11-09",
      audio: { seconds: 0, last: 20 },
      screen: { seconds: 4, last: 99 },
      browsers: { total: 3, last: 5 },
      obsidian: { total: 0, last: null },
    };
    const rows = counterFacts(document);
    expect(rows).not.toBeNull();
    const bySource = Object.fromEntries(rows!.map((row) => [row.source, row]));
    expect(bySource.audio.nothingYet).toBe(false);
    expect(bySource.audio.last).toBe(20);
    expect(bySource.audio.pageIncludesStarted).toBe(true);
    expect(bySource.screen.last).toBe(99);
    expect(bySource.browsers.last).toBe(5);
    expect(bySource.obsidian.nothingYet).toBe(true);
    expect(bySource.obsidian.last).toBeNull();
    expect(bySource.obsidian.pageIncludesStarted).toBe(false);
  });
});

describe("deck and shelf", () => {
  it("lists the four sources and then add more", () => {
    expect(deckTiles()).toEqual(["audio", "screen", "browsers", "obsidian", "addMore"]);
    expect(addMoreSources()).toEqual(["audio", "screen", "browsers", "obsidian"]);
  });

  it("badges the shelf row a notice belongs to", () => {
    expect(
      shelfBadges({ unknownJournal: false, shortcutNeedsRepair: false, updateAvailable: false }),
    ).toEqual([]);
    expect(
      shelfBadges({ unknownJournal: true, shortcutNeedsRepair: true, updateAvailable: true }),
    ).toEqual(["journal", "thisPc", "updates"]);
  });

  it("space toggles a switch, skips browsers, and opens Obsidian to turn it on", () => {
    expect(spaceOnTile("audio", true)).toBe("toggle");
    expect(spaceOnTile("screen", false)).toBe("toggle");
    expect(spaceOnTile("browsers", null)).toBe("none");
    expect(spaceOnTile("obsidian", false)).toBe("open");
    expect(spaceOnTile("obsidian", true)).toBe("toggle");
    expect(spaceOnTile("obsidian", null)).toBe("none");
  });
});

describe("greeting", () => {
  it("follows the hour bands", () => {
    expect(greeting(5)).toBe("good morning");
    expect(greeting(11)).toBe("good morning");
    expect(greeting(12)).toBe("good afternoon");
    expect(greeting(16)).toBe("good afternoon");
    expect(greeting(17)).toBe("good evening");
    expect(greeting(4)).toBe("good evening");
  });
});
