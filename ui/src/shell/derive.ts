// Pure decisions for the desktop shell. No DOM and no commands.

export type StateWord =
  | "off"
  | "on"
  | "paused"
  | "setting up"
  | "ready to set up"
  | "checking…"
  | "needs attention";

export type SourceId = "audio" | "screen" | "browsers" | "obsidian" | "screenshots";

export type WidthClass = "full" | "narrower" | "phone";

export type ShelfRow = "home" | "journal" | "thisPc" | "updates" | "help";

export type Pane =
  | "status"
  | "audio"
  | "screen"
  | "browsers"
  | "obsidian"
  | "screenshots"
  | "import"
  | "screenLeftOut";

const SOURCE_ORDER: readonly SourceId[] = ["audio", "screen", "browsers", "obsidian", "screenshots"];

export function widthClass(innerWidth: number): WidthClass {
  if (innerWidth >= 1100) return "full";
  if (innerWidth >= 720) return "narrower";
  return "phone";
}

export function deckColumns(width: WidthClass): 2 | 3 {
  if (width === "full") return 3;
  return 2;
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
  /** Screen is on and activity is on. The on sub-line names activity. */
  activityAlongside: boolean;
  /** Audio is on and a microphone is taking something in. The sub-line names that microphone. */
  namesMicrophone: boolean;
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
  // A missing microphone with nothing else taking in is steady, not startup.
  else if (!input.micDevicePresent) word = "needs attention";
  else word = "setting up";
  const namesMicrophone =
    word === "on" && input.micEnabled && input.micDevicePresent && input.micTakingIn;
  return {
    word,
    switchOn: eitherOn,
    activityKept: false,
    activityAlongside: false,
    namesMicrophone,
  };
}

export type ScreenTileInput = {
  enabled: boolean;
  accessDenied: boolean;
  failed: boolean;
  activityFailed: boolean;
  activityOn: boolean;
  /** Activity is really going in right now, not just switched on. */
  activityRunning: boolean;
  appPaused: boolean;
  running: boolean;
  permissionAsked: boolean;
};

export function screenTile(input: ScreenTileInput): TileView {
  const fault = input.accessDenied || input.failed || input.activityFailed;
  const activityKept = input.activityRunning && (!input.enabled || input.appPaused);
  let word: StateWord;
  if (fault) word = "needs attention";
  else if (!input.enabled) word = "off";
  else if (input.appPaused) word = "paused";
  else if (input.running) word = "on";
  else if (!input.permissionAsked) word = "ready to set up";
  else word = "setting up";
  return {
    word,
    switchOn: input.enabled,
    activityKept,
    // A paused tile keeps its own line, so activity is named there too unless it is still going.
    activityAlongside: (word === "on" || word === "paused") && input.activityOn && !activityKept,
    namesMicrophone: false,
  };
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
  return {
    word,
    switchOn: null,
    activityKept: false,
    activityAlongside: false,
    namesMicrophone: false,
  };
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
  /** The switch was turned on and the folder is still needed. */
  armed?: boolean;
}): TileView {
  let word: StateWord;
  if (input.state === "needs_attention") word = "needs attention";
  else if (input.state === "paused") word = "paused";
  else if (input.state === "on") word = "on";
  else if (input.state === "waiting_for_journal_update" || input.state === "confirm") word = "setting up";
  else if (input.state === "checking") word = "checking…";
  else if (input.armed && input.vault === null) word = "ready to set up";
  else word = "off";
  const switchOn =
    word === "off" ? false : word === "on" || word === "paused" || word === "ready to set up" ? true : null;
  return {
    word,
    switchOn,
    activityKept: false,
    activityAlongside: false,
    namesMicrophone: false,
  };
}

export type ScreenshotsStateName =
  | "not_set_up"
  | "off"
  | "no_folder"
  | "folder_missing"
  | "choosing"
  | "bringing_in"
  | "on"
  | "paused";

/** The screenshots tile's word and switch. Not yet reported reads as off. */
export function screenshotsTile(state: ScreenshotsStateName | null): TileView {
  let word: StateWord;
  if (state === "folder_missing") word = "needs attention";
  else if (state === "not_set_up" || state === "no_folder" || state === "choosing") word = "ready to set up";
  else if (state === "paused") word = "paused";
  else if (state === "on" || state === "bringing_in") word = "on";
  else word = "off";
  return {
    word,
    switchOn: state !== null && state !== "off" && state !== "not_set_up" && state !== "no_folder",
    activityKept: false,
    activityAlongside: false,
    namesMicrophone: false,
  };
}

export type ScreenshotsReason =
  | "noFolder"
  | "folderGone"
  | "countFailed"
  | "paused"
  | "waitingForJournal"
  | "bringingIn";

export type ScreenshotsAction = "tryAgain" | "resume";

export type ScreenshotsPageInput = {
  state: ScreenshotsStateName;
  countFailed: boolean;
  waitingForJournal: boolean;
  windowsFolder: boolean;
  hasFolder: boolean;
  confirmed: boolean;
};

export type ScreenshotsPage = {
  reason: ScreenshotsReason | null;
  /** The state's one action. Choosing a folder is never it: that sits by the folder fact. */
  action: ScreenshotsAction | null;
  chooser: boolean;
  /** Other files stay where they are: a folder the owner chose. */
  otherFilesStay: boolean;
  /** Every picture comes in: Windows' own folder. Otherwise only what can be told is a screenshot. */
  everyPicture: boolean;
  /** The folder fact and its chooser, in every state. */
  folderFact: true;
  /** What can't come in is said once there is something confirmed. */
  finalLine: boolean;
};

/** What the screenshots page shows, in the page order. */
export function screenshotsPage(input: ScreenshotsPageInput): ScreenshotsPage {
  let reason: ScreenshotsReason | null = null;
  let action: ScreenshotsAction | null = null;
  if (input.state === "no_folder") reason = "noFolder";
  else if (input.state === "folder_missing") reason = "folderGone";
  else if (input.state === "choosing" && input.countFailed) {
    reason = "countFailed";
    action = "tryAgain";
  } else if (input.state === "paused") {
    reason = "paused";
    action = "resume";
  } else if ((input.state === "on" || input.state === "bringing_in") && input.waitingForJournal) {
    reason = "waitingForJournal";
  } else if (input.state === "bringing_in") reason = "bringingIn";
  return {
    reason,
    action,
    chooser: input.state === "choosing",
    otherFilesStay: input.hasFolder && !input.windowsFolder,
    everyPicture: input.hasFolder && input.windowsFolder,
    folderFact: true,
    finalLine: input.confirmed,
  };
}

/** What the confirm button carries: a count, today, or the wait for the count. */
export function screenshotsConfirm(input: {
  span: string;
  count: number | null;
  counting: boolean;
}): { kind: "checking" } | { kind: "today" } | { kind: "count"; count: number } {
  if (input.counting || input.count === null) return { kind: "checking" };
  if (input.span === "nothing" || input.count === 0) return { kind: "today" };
  return { kind: "count", count: input.count };
}

export type DeckTileId = SourceId | "import" | "addMore";

/** Sources, the file-import destination, then add more. */
export function deckTiles(): readonly DeckTileId[] {
  return [...SOURCE_ORDER, "import", "addMore"];
}

/** The add-more page lists the sources already on the deck, in that order, as one section. */
export function addMoreSources(): readonly SourceId[] {
  return SOURCE_ORDER;
}

export type ShelfNotice = "journal" | "thisPc" | "updates";

/** A notice that belongs to no tile badges its shelf row. Home and help never badge. */
export function shelfBadges(input: {
  unknownJournal: boolean;
  journalOffline: boolean;
  shortcutNeedsRepair: boolean;
  updateAvailable: boolean;
}): ShelfNotice[] {
  const rows: ShelfNotice[] = [];
  if (input.unknownJournal || input.journalOffline) rows.push("journal");
  if (input.shortcutNeedsRepair) rows.push("thisPc");
  if (input.updateAvailable) rows.push("updates");
  return rows;
}

/** Space toggles a switch. Browsers has none. Obsidian's on action opens the page. Screenshots' switch turns it on and opens the page. */
export function spaceOnTile(source: SourceId, switchOn: boolean | null): "toggle" | "open" | "none" {
  if (source === "browsers" || switchOn === null) return "none";
  if (source === "obsidian") return switchOn ? "toggle" : "open";
  return "toggle";
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
  importsNeedOwner?: boolean;
};

/** Audio's companion line. Computer sound wins over a microphone name, on or paused. */
export function audioSubline(input: {
  systemAudioOn: boolean;
  word: StateWord;
  microphone: string | null;
}): string {
  if ((input.word === "on" || input.word === "paused") && input.systemAudioOn) {
    return "with computer sound";
  }
  if (input.word === "on" && input.microphone) return input.microphone;
  return "";
}

/** The tile line under the state word. A fault is never a generic companion. */
export function sourceSubline(input: {
  word: StateWord;
  paired: boolean;
  companion: string;
  setup: string;
  attention: string;
  /** What this source is waiting on while it sets up, when it knows. */
  settingUp?: string;
}): string {
  if (input.word === "needs attention") return input.attention;
  if (input.word === "off") {
    return "turn it on any time.";
  }
  if (input.word === "setting up") {
    if (input.settingUp) return input.settingUp;
    return input.paired ? "getting ready. connecting to your journal." : "getting ready…";
  }
  if (input.word === "ready to set up") return input.setup;
  return input.companion;
}

/** The about row's value. The unknown line does not repeat the word journal. */
export function aboutJournalValue(line: string): string {
  return line === "journal unknown" ? "unknown" : line;
}

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

export type StatusLead = {
  headline: string;
  lines: readonly string[];
};

/**
 * Status-pane lead. Caught up is the phone's two lines.
 * Offline with a count leads with the pill's words, then where it waits.
 * A quiet offline lead is the word only: no zero, and nothing is on the device.
 */
export function statusLead(input: JournalPillInput): StatusLead {
  const caughtUp =
    input.paired &&
    !input.needsNewLink &&
    !input.refused &&
    !input.syncPaused &&
    !input.held &&
    !input.offline &&
    !input.connecting &&
    !input.waitingToSync &&
    !input.importsNeedOwner;
  if (caughtUp) {
    return { headline: "all caught up", lines: ["everything is in your journal"] };
  }
  if (input.paired && input.offline) {
    if (input.waiting > 0) {
      return { headline: `offline · ${input.waiting} waiting`, lines: ["on this device"] };
    }
    return { headline: "offline", lines: [] };
  }
  if (input.paired && input.waitingToSync) {
    return { headline: "waiting to sync", lines: ["on this device"] };
  }
  return { headline: "", lines: [] };
}

export type JournalLinkWord = "connected" | "connecting" | "offline";

export type JournalFact = {
  label: "connection" | "last reached";
  value: string;
};

/** Connection, and last reached only when a pair has a real instant. */
export function journalFacts(input: {
  paired: boolean;
  link: JournalLinkWord | null;
  lastReached: string | null;
}): JournalFact[] {
  if (!input.paired) return [];
  const facts: JournalFact[] = [];
  if (input.link === "connected") facts.push({ label: "connection", value: "connected" });
  if (input.link === "connecting") facts.push({ label: "connection", value: "connecting…" });
  if (input.link === "offline") facts.push({ label: "connection", value: "not reaching your journal right now" });
  if (input.lastReached) facts.push({ label: "last reached", value: input.lastReached });
  return facts;
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
    case "screenshots":
    case "import":
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

export type CounterSourceName = "audio" | "screen" | "browsers" | "obsidian" | "screenshots";

export type CounterDocument = {
  started: string;
  audio: { seconds: number; last: number | null };
  screen: { seconds: number; last: number | null };
  browsers: { total: number; last: number | null };
  obsidian: { total: number; last: number | null };
  /** Absent from a document written before screenshots were counted. */
  screenshots?: { total: number; last: number | null };
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

const MONTHS = [
  "january",
  "february",
  "march",
  "april",
  "may",
  "june",
  "july",
  "august",
  "september",
  "october",
  "november",
  "december",
] as const;

/** A stored date, month and day, with the year only when it is not `nowYear`. */
export function startedLabel(isoDate: string, nowYear: number): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate);
  if (!match) return isoDate;
  const year = Number(match[1]);
  const month = MONTHS[Number(match[2]) - 1];
  const day = Number(match[3]);
  if (!month || !day) return isoDate;
  return year === nowYear ? `${month} ${day}` : `${month} ${day}, ${year}`;
}

/** Hours, or minutes under an hour, or the under-a-minute band. */
export function durationTotal(seconds: number): string {
  if (seconds < 60) return "less than a minute";
  if (seconds < 3600) {
    const minutes = Math.floor(seconds / 60);
    return minutes === 1 ? "1 minute" : `${minutes.toLocaleString("en-US")} minutes`;
  }
  const hours = Math.floor(seconds / 3600);
  return hours === 1 ? "1 hour" : `${hours.toLocaleString("en-US")} hours`;
}

/** A counted total. One uses `one`, any other count uses `many`. */
export function countTotal(count: number, one: string, many: string): string {
  return count === 1 ? `1 ${one}` : `${count.toLocaleString("en-US")} ${many}`;
}

/**
 * How long since `unixSecs`. Under a minute, then minutes, then hours, then
 * yesterday, then the date.
 */
export function relativeTaken(unixSecs: number, nowSecs: number): string {
  const delta = Math.max(0, nowSecs - unixSecs);
  if (delta < 60) return "just now";
  if (delta < 3600) {
    const minutes = Math.floor(delta / 60);
    return `${minutes} min ago`;
  }
  if (delta < 86_400) {
    const hours = Math.floor(delta / 3600);
    return hours === 1 ? "1 hour ago" : `${hours} hours ago`;
  }
  if (delta < 172_800) return "yesterday";
  return startedLabel(new Date(unixSecs * 1000).toISOString().slice(0, 10), new Date(nowSecs * 1000).getUTCFullYear());
}

/** `null` is an absent, unreadable, or failed read: no rows, and not `nothing yet`. */
export function counterFacts(document: CounterDocument | null): SourceFacts[] | null {
  if (document === null) return null;
  const rows: Array<[CounterSourceName, number, number | null]> = [
    ["audio", document.audio.seconds, document.audio.last],
    ["screen", document.screen.seconds, document.screen.last],
    ["browsers", document.browsers.total, document.browsers.last],
    ["obsidian", document.obsidian.total, document.obsidian.last],
    ["screenshots", document.screenshots?.total ?? 0, document.screenshots?.last ?? null],
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
