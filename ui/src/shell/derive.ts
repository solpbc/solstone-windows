// Pure decisions for the desktop shell. No DOM and no commands.

export type StateWord =
  | "off"
  | "on"
  | "paused"
  | "setting up"
  | "ready to set up"
  | "checking…"
  | "needs attention";

export type SourceId = "audio" | "screen" | "browsers" | "obsidian";

export type WidthClass = "full" | "narrower" | "phone";

export type ShelfRow = "home" | "journal" | "thisPc" | "updates" | "help";

export type Pane =
  | "status"
  | "audio"
  | "screen"
  | "browsers"
  | "obsidian"
  | "screenLeftOut";

const SOURCE_ORDER: readonly SourceId[] = ["audio", "screen", "browsers", "obsidian"];

export function widthClass(innerWidth: number): WidthClass {
  if (innerWidth >= 1100) return "full";
  if (innerWidth >= 720) return "narrower";
  return "phone";
}

export function deckColumns(width: WidthClass): 1 | 2 | 3 {
  if (width === "full") return 3;
  if (width === "narrower") return 2;
  return 1;
}

export function greeting(hour: number): "good morning" | "good afternoon" | "good evening" {
  if (hour >= 5 && hour <= 11) return "good morning";
  if (hour >= 12 && hour <= 16) return "good afternoon";
  return "good evening";
}

/** Stored bit when present. Otherwise the live computer-sound key if the microphone is on, else true. */
export function derivedComputerSoundWanted(
  micEnabled: boolean,
  systemAudioEnabled: boolean,
  stored: boolean | null,
): boolean {
  if (stored !== null) return stored;
  if (micEnabled) return systemAudioEnabled;
  return true;
}

export type AudioTileInput = {
  micEnabled: boolean;
  micAccessDenied: boolean;
  micDevicePresent: boolean;
  micTakingIn: boolean;
  systemAudioEnabled: boolean;
  systemAudioFaulted: boolean;
  systemAudioTakingIn: boolean;
  appPaused: boolean;
  wanted: boolean | null;
};

export type TileView = {
  word: StateWord;
  /** The tile's switch is on. Browsers never has one. */
  switchOn: boolean | null;
  /** Screen is off or paused while activity is still going in. A fact, not a sentence. */
  activityKept: boolean;
};

export function audioTile(input: AudioTileInput): TileView {
  const micLive = input.micEnabled && input.micDevicePresent;
  const soundLive = input.systemAudioEnabled;
  const eitherOn = micLive || soundLive;
  const fault =
    (input.micEnabled && input.micAccessDenied) ||
    (input.systemAudioEnabled && input.systemAudioFaulted);
  let word: StateWord;
  if (fault) word = "needs attention";
  else if (!eitherOn) word = "off";
  else if (input.appPaused) word = "paused";
  else if (input.micTakingIn || input.systemAudioTakingIn) word = "on";
  else word = "setting up";
  return { word, switchOn: eitherOn, activityKept: false };
}

export type ScreenTileInput = {
  enabled: boolean;
  accessDenied: boolean;
  failed: boolean;
  activityFailed: boolean;
  activityOn: boolean;
  appPaused: boolean;
  running: boolean;
  permissionAsked: boolean;
};

export function screenTile(input: ScreenTileInput): TileView {
  const fault = input.accessDenied || input.failed || input.activityFailed;
  const activityKept = input.activityOn && (!input.enabled || input.appPaused);
  let word: StateWord;
  if (fault) word = "needs attention";
  else if (!input.enabled) word = "off";
  else if (input.appPaused) word = "paused";
  else if (input.running) word = "on";
  else if (!input.permissionAsked) word = "ready to set up";
  else word = "setting up";
  return { word, switchOn: input.enabled, activityKept };
}

export type BrowserCapture = "unavailable" | "not_paired" | "permitted" | "paused" | "intake_off";

export function browsersTile(input: {
  capture: BrowserCapture;
  deliveryFailed: boolean;
  appPaused: boolean;
}): TileView {
  let word: StateWord;
  if (input.capture === "intake_off" || input.deliveryFailed) word = "needs attention";
  else if (input.capture === "not_paired" || input.capture === "unavailable") word = "ready to set up";
  else if (input.capture === "paused" || input.appPaused) word = "paused";
  else word = "on";
  return { word, switchOn: null, activityKept: false };
}

export type ObsidianStateName =
  | "off"
  | "checking"
  | "confirm"
  | "on"
  | "paused"
  | "waiting_for_journal_update"
  | "needs_attention";

export function obsidianTile(input: {
  state: ObsidianStateName;
  vault: string | null;
  foundCount: number;
}): TileView {
  let word: StateWord;
  if (input.state === "needs_attention") word = "needs attention";
  else if (input.state === "paused") word = "paused";
  else if (input.state === "on") word = "on";
  else if (input.state === "waiting_for_journal_update" || input.state === "confirm") word = "setting up";
  else if (input.state === "checking") word = "checking…";
  else if (input.vault === null && input.foundCount === 0) word = "ready to set up";
  else word = "off";
  const switchOn = word === "off" || word === "ready to set up" ? false : word === "on" || word === "paused" ? true : null;
  return { word, switchOn, activityKept: false };
}

export type JournalPillInput = {
  paired: boolean;
  needsNewLink: boolean;
  refused: boolean;
  syncPaused: boolean;
  held: boolean;
  offline: boolean;
  connecting: boolean;
  waitingToSync: boolean;
  waiting: number;
  sourceNeedsAttention: boolean;
};

/** Journal words only. `sourceNeedsAttention` is ignored on purpose. */
export function journalPill(input: JournalPillInput): string {
  const n = input.waiting;
  const count = n === 0 ? "" : ` · ${n} waiting`;
  if (!input.paired) return "not paired";
  if (input.needsNewLink) return "your journal needs a new link";
  if (input.refused) return n === 0 ? "pair again" : `pair again${count}`;
  if (input.syncPaused) return n === 0 ? "sync paused" : `sync paused${count}`;
  if (input.held) return n === 0 ? "confirm the mark" : `confirm the mark${count}`;
  if (input.offline) return `offline${count}`;
  if (input.connecting) return "connecting…";
  if (input.waitingToSync) return "waiting to sync";
  return "connected";
}

export type PauseTimeLeft =
  | { kind: "untilResume" }
  | { kind: "lessThanAMinute" }
  | { kind: "minutes"; minutes: number };

/** `secondsLeft` null is indefinite. Otherwise whole minutes, rounded up, under a minute on its own. */
export function pauseTimeLeft(secondsLeft: number | null): PauseTimeLeft {
  if (secondsLeft === null) return { kind: "untilResume" };
  if (secondsLeft < 60) return { kind: "lessThanAMinute" };
  return { kind: "minutes", minutes: Math.ceil(secondsLeft / 60) };
}

export function headerWhilePaused(paused: boolean): {
  timeLeft: boolean;
  resume: boolean;
  pauseDurations: boolean;
} {
  return paused
    ? { timeLeft: true, resume: true, pauseDurations: false }
    : { timeLeft: false, resume: false, pauseDurations: false };
}

export type MenuRowKind =
  | "attention"
  | "status"
  | "pause"
  | "resume"
  | "openSettings"
  | "openJournal"
  | "about"
  | "quit";

export type MenuRow = {
  kind: MenuRowKind;
  source?: SourceId;
  disabled?: boolean;
};

export function menuRows(input: {
  needsAttention: readonly SourceId[];
  paused: boolean;
  paired: boolean;
  journalReachable: boolean;
}): MenuRow[] {
  const rows: MenuRow[] = [];
  const first = SOURCE_ORDER.find((source) => input.needsAttention.includes(source));
  if (first) rows.push({ kind: "attention", source: first });
  rows.push({ kind: "status", disabled: true });
  rows.push(input.paused ? { kind: "resume" } : { kind: "pause" });
  rows.push({ kind: "openSettings" });
  if (input.paired) rows.push({ kind: "openJournal", disabled: !input.journalReachable });
  rows.push({ kind: "about" }, { kind: "quit" });
  return rows;
}

export type RestartPlace = "audio" | "screen" | "technicalDetails" | "absent";

export function restartPlace(input: {
  phaseError: boolean;
  micFaulted: boolean;
  systemAudioFaulted: boolean;
  screenFaulted: boolean;
  activityFaulted: boolean;
  storageFault: boolean;
}): RestartPlace {
  if (!input.phaseError) return "absent";
  if (input.micFaulted || input.systemAudioFaulted) return "audio";
  if (input.screenFaulted || input.activityFaulted) return "screen";
  if (input.storageFault) return "technicalDetails";
  return "absent";
}

/** `settings://navigate` payload. `sources` is the deck with status. A source id opens that page. */
export function settingsNavigate(payload: string): { row: ShelfRow; pane: Pane } {
  switch (payload) {
    case "audio":
    case "screen":
    case "browsers":
    case "obsidian":
      return { row: "home", pane: payload };
    case "sources":
    case "home":
      return { row: "home", pane: "status" };
    default:
      return legacyRoute(payload);
  }
}

export function legacyRoute(route: string): { row: ShelfRow; pane: Pane } {
  switch (route) {
    case "home":
    case "sources":
      return { row: "home", pane: "status" };
    case "privacy":
      return { row: "home", pane: "screenLeftOut" };
    case "journal":
      return { row: "journal", pane: "status" };
    case "shortcut":
    case "storage":
      return { row: "thisPc", pane: "status" };
    case "updates":
      return { row: "updates", pane: "status" };
    case "help":
      return { row: "help", pane: "status" };
    default:
      return { row: "home", pane: "status" };
  }
}

export function statusRowOpens(source: SourceId): Pane {
  return source;
}

export type CounterSourceName = "audio" | "screen" | "browsers" | "obsidian";

export type CounterDocument = {
  started: string;
  audio: { seconds: number; last: number | null };
  screen: { seconds: number; last: number | null };
  browsers: { total: number; last: number | null };
  obsidian: { total: number; last: number | null };
};

export type SourceFacts = {
  source: CounterSourceName;
  nothingYet: boolean;
  total: number;
  last: number | null;
  started: string | null;
  /** Page `so far` carries the started date. A status cell does not. */
  pageIncludesStarted: boolean;
};

/** `null` is an absent, unreadable, or failed read: no rows, and not `nothing yet`. */
export function counterFacts(document: CounterDocument | null): SourceFacts[] | null {
  if (document === null) return null;
  const rows: Array<[CounterSourceName, number, number | null]> = [
    ["audio", document.audio.seconds, document.audio.last],
    ["screen", document.screen.seconds, document.screen.last],
    ["browsers", document.browsers.total, document.browsers.last],
    ["obsidian", document.obsidian.total, document.obsidian.last],
  ];
  return rows.map(([source, total, last]) => {
    const nothingYet = total === 0 && last === null;
    return {
      source,
      nothingYet,
      total,
      last: nothingYet ? null : last,
      started: nothingYet ? null : document.started,
      pageIncludesStarted: !nothingYet,
    };
  });
}
