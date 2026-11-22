import { t, stateWord, formatLocale, locale, number } from "../i18n";
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

export function greeting(hour: number): string {
  if (hour >= 5 && hour <= 11) return t("windows.ui.goodmorning");
  if (hour >= 12 && hour <= 16) return t("windows.ui.goodafternoon");
  return t("windows.ui.goodevening");
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
  importsReadyToReview?: number;
};

export type JournalPillKind = "connected" | "review" | "neutral";

/**
 * Pill indicator visual kind.
 * Green dot ("connected") only when paired, connected, nothing waiting, no import faults,
 * and no imports awaiting review. Orange bullseye ("review") when imports await review and
 * no faults exist. Neutral ring ("neutral") otherwise (including import faults when waiting == 0).
 */
export function journalPillVisual(input: JournalPillInput): JournalPillKind {
  if (
    !input.paired ||
    input.needsNewLink ||
    input.refused ||
    input.syncPaused ||
    input.held ||
    input.offline ||
    input.connecting ||
    input.waitingToSync
  ) {
    return "neutral";
  }
  if (input.importsNeedOwner) return "neutral";
  if ((input.importsReadyToReview ?? 0) > 0) return "review";
  return "connected";
}

/** Audio's companion line. Computer sound wins over a microphone name, on or paused. */
export function audioSubline(input: {
  systemAudioOn: boolean;
  word: StateWord;
  microphone: string | null;
}): string {
  if ((input.word === "on" || input.word === "paused") && input.systemAudioOn) {
    return t("windows.ui.withcomputersound");
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
    return t("windows.ui.turnitonanytime");
  }
  if (input.word === "setting up") {
    if (input.settingUp) return input.settingUp;
    return input.paired ? t("windows.ui.gettingreadyconnectingtoyourjournal") : t("windows.ui.gettingready");
  }
  if (input.word === "ready to set up") return input.setup;
  return input.companion;
}

/** The about row's value. The unknown line does not repeat the word journal. */
export function aboutJournalValue(line: string): string {
  return line === t("windows.about.unknown") ? t("windows.ui.unknown") : line;
}

/** Journal words only. `sourceNeedsAttention` is ignored on purpose. */
export function journalPill(input: JournalPillInput): string {
  const n = input.waiting;
  if (!input.paired) return t("windows.desktop.shared.app.notpaired");
  if (input.needsNewLink) return t("windows.ui.yourjournalneedsanewlink");
  if (input.refused) return n === 0 ? t("windows.pill.refused") : t("windows.pill.refusedwaiting", { waiting: String(number(n, {useGrouping:false})) });
  if (input.syncPaused) return n === 0 ? t("windows.pill.syncpaused") : t("windows.pill.syncpausedwaiting", { waiting: String(number(n, {useGrouping:false})) });
  if (input.held) return n === 0 ? t("windows.pill.held") : t("windows.pill.heldwaiting", { waiting: String(number(n, {useGrouping:false})) });
  if (input.offline) return n === 0 ? t("windows.pill.offline") : t("windows.pill.offlinewaiting", { waiting: String(number(n, {useGrouping:false})) });
  if (input.connecting) return t("windows.ui.connecting");
  if (input.waitingToSync) return t("windows.ui.waitingtosync");
  const ready = input.importsReadyToReview ?? 0;
  if (!input.importsNeedOwner && ready > 0) {
    return t("windows.pill.readytoreview", { display: number(ready, { useGrouping: false }) });
  }
  return t("windows.ui.connected");
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
  const ready = input.importsReadyToReview ?? 0;
  const connected =
    input.paired &&
    !input.needsNewLink &&
    !input.refused &&
    !input.syncPaused &&
    !input.held &&
    !input.offline &&
    !input.connecting &&
    !input.waitingToSync;
  if (connected && !input.importsNeedOwner && ready === 0) {
    return { headline: t("windows.ui.allcaughtup"), lines: [t("windows.ui.everythingisinyourjournal")] };
  }
  if (connected && input.importsNeedOwner) {
    return {
      headline: t("windows.import.statusattentionheadline"),
      lines: [t("windows.import.statusattentionsub")],
    };
  }
  if (connected && ready > 0) {
    return {
      headline: t("windows.import.statusreviewheadline", {
        count: ready,
        display: number(ready, { useGrouping: false }),
      }),
      lines: [t("windows.import.statusreviewsub")],
    };
  }
  if (input.paired && input.offline) {
    if (input.waiting > 0) {
      return { headline: t("windows.ui.offlinearg0waiting", { "arg0": String(input.waiting) }), lines: [t("windows.ui.onthisdevice")] };
    }
    return { headline: t("windows.ui.offline"), lines: [] };
  }
  if (input.paired && input.waitingToSync) {
    return { headline: t("windows.ui.waitingtosync"), lines: [t("windows.ui.onthisdevice")] };
  }
  return { headline: "", lines: [] };
}

export type JournalLinkWord = "connected" | "connecting" | "offline";

export type JournalFact = {
  label: string;
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
  if (input.link === "connected") facts.push({ label: t("windows.ui.connection"), value: t("windows.ui.connected") });
  if (input.link === "connecting") facts.push({ label: t("windows.ui.connection"), value: t("windows.ui.connecting") });
  if (input.link === "offline") facts.push({ label: t("windows.ui.connection"), value: t("windows.ui.notreachingyourjournalrightnow") });
  if (input.lastReached) facts.push({ label: t("windows.ui.lastreached"), value: input.lastReached });
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

export type CounterSourceName = "audio" | "screen" | "browsers" | "obsidian" | "screenshots" | "import";

export type CounterDocument = {
  started: string;
  audio: { seconds: number; last: number | null };
  screen: { seconds: number; last: number | null };
  browsers: { total: number; last: number | null };
  obsidian: { total: number; last: number | null };
  /** Absent from a document written before screenshots were counted. */
  screenshots?: { total: number; last: number | null };
  /** Absent from a document written before imports were counted. */
  imports?: { total: number; last: number | null };
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
  t("windows.ui.january"),
  t("windows.ui.february"),
  t("windows.ui.march"),
  t("windows.ui.april"),
  t("windows.ui.may"),
  t("windows.ui.june"),
  t("windows.ui.july"),
  t("windows.ui.august"),
  t("windows.ui.september"),
  t("windows.ui.october"),
  t("windows.ui.november"),
  t("windows.ui.december"),
] as const;

/** A stored date, month and day, with the year only when it is not `nowYear`. */
export function startedLabel(isoDate: string, nowYear: number): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate);
  if (!match) return isoDate;
  const year = Number(match[1]);
  const month = MONTHS[Number(match[2]) - 1];
  const day = Number(match[3]);
  if (!month || !day) return isoDate;
  if (locale !== "en-US") return new Intl.DateTimeFormat(formatLocale, {month:"long",day:"numeric", ...(year === nowYear ? {} : {year:"numeric"})}).format(new Date(year, Number(match[2])-1, day));
  return year === nowYear ? t("windows.tray.text", { "tooltip": String(month), "notice": String(day) }) : t("windows.ui.arg0arg1arg2", { "arg0": String(month), "arg1": String(day), "arg2": String(year) });
}

/** Hours, or minutes under an hour, or the under-a-minute band. */
export function durationTotal(seconds: number): string {
  if (seconds < 60) return t("windows.tray.lessthanaminute");
  return seconds < 3600 ? quantity("minutes", Math.floor(seconds / 60)) : quantity("hours", Math.floor(seconds / 3600));
}

/** A counted total. One uses `one`, any other count uses `many`. */
export function quantity(
  kind: "pages" | "notes" | "screenshots" | "items" | "minutes" | "hours",
  count: number,
): string {
  return t(`windows.quantity.${kind}`, { count, display: number(count) });
}

export function countTotal(count: number, one: string, many: string): string {
  return `${number(count)} ${count === 1 ? one : many}`;
}

/**
 * How long since `unixSecs`. Under a minute, then minutes, then hours, then
 * yesterday, then the date.
 */
export function relativeTaken(unixSecs: number, nowSecs: number): string {
  const delta = Math.max(0, nowSecs - unixSecs);
  if (delta < 60) return t("windows.desktop.shared.app.justnow");
  if (delta < 3600) {
    const minutes = Math.floor(delta / 60);
    return t("windows.ui.arg0minago", { "arg0": String(minutes) });
  }
  if (delta < 86_400) {
    const hours = Math.floor(delta / 3600);
    return t("windows.time.takenhours", {count: hours, display: number(hours, {useGrouping:false})});
  }
  if (delta < 172_800) return t("windows.ui.yesterday");
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
    ["import", document.imports?.total ?? 0, document.imports?.last ?? null],
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

/** Strict ASCII `YYYYMMDD_HHMMSS` check matching `observer_model::import_id::is_valid_import_id`. */
export function isValidImportId(value: string | null | undefined): value is string {
  return typeof value === "string" && /^[0-9]{8}_[0-9]{6}$/.test(value);
}

export type ImportRowState =
  | "ready_to_review"
  | "review_expired"
  | "refused"
  | "sending"
  | "waiting";

export type ImportTileRow = {
  name: string;
  state: ImportRowState;
};

export type ImportTileInput = {
  rows: readonly ImportTileRow[];
  /** Queue is held because unpaired, mark held, pairing refused, offline, or journal too old. */
  waitingForJournal: boolean;
  /** Document-level attention even without individual refused rows (e.g. interrupted intake, unavailable store). */
  needsAttention: boolean;
  /** Active local intake copying progress. */
  copying?: { done: number; total: number } | null;
};

export type ImportTileView = {
  word: "" | "waiting" | "sending" | "ready to review" | "needs attention";
  sub: string;
  ariaLabel?: string;
};

/**
 * Pure home `import` tile derivation.
 * Quiet when idle (`word: ""`, `sub: "files"`).
 * Active/held follows worst-signal-wins: `needs attention` > `ready to review` > `sending` > `waiting`.
 */
export function importTileView(input: ImportTileInput): ImportTileView {
  const attentionRows = input.rows.filter(
    (r) => r.state === "refused" || r.state === "review_expired",
  );
  const reviewRows = input.rows.filter((r) => r.state === "ready_to_review");
  const sendingRows = input.rows.filter((r) => r.state === "sending");
  const waitingRows = input.rows.filter((r) => r.state === "waiting");

  if (attentionRows.length > 0 || input.needsAttention) {
    const word = "needs attention" as const;
    let sub: string;
    if (attentionRows.length === 1) {
      sub = attentionRows[0].name;
    } else if (attentionRows.length > 1) {
      sub = quantity("items", attentionRows.length);
    } else if (waitingRows.length === 1) {
      sub = waitingRows[0].name;
    } else if (waitingRows.length > 1) {
      sub = quantity("items", waitingRows.length);
    } else {
      sub = t("windows.ui.files");
    }
    return {
      word,
      sub,
      ariaLabel: t("windows.import.tilea11y", { state: stateWord(word), sub }),
    };
  }

  if (reviewRows.length > 0) {
    const word = "ready to review" as const;
    const sub =
      reviewRows.length === 1
        ? reviewRows[0].name
        : t("windows.import.tilereadycount", {
            display: number(reviewRows.length, { useGrouping: false }),
          });
    return {
      word,
      sub,
      ariaLabel: t("windows.import.tilea11y", { state: stateWord(word), sub }),
    };
  }

  if (sendingRows.length > 0) {
    const word = "sending" as const;
    const totalActive = sendingRows.length + waitingRows.length;
    const sub =
      totalActive === 1
        ? sendingRows[0].name
        : t("windows.import.tilesendingcount", {
            count: totalActive,
            display: number(totalActive, { useGrouping: false }),
          });
    return {
      word,
      sub,
      ariaLabel: t("windows.import.tilea11y", { state: stateWord(word), sub }),
    };
  }

  const copyingRemaining = input.copying
    ? Math.max(0, input.copying.total - input.copying.done)
    : 0;
  const totalWaiting = waitingRows.length + copyingRemaining;
  if (totalWaiting > 0) {
    const word = "waiting" as const;
    const sub = input.waitingForJournal
      ? t("windows.import.tilewaitingjournal")
      : t("windows.import.tilewaitingcount", {
          display: number(totalWaiting, { useGrouping: false }),
        });
    return {
      word,
      sub,
      ariaLabel: t("windows.import.tilea11y", { state: stateWord(word), sub }),
    };
  }

  return { word: "", sub: t("windows.ui.files") };
}

function importRowRank(state: string): number {
  switch (state) {
    case "ready_to_review":
      return 0;
    case "review_expired":
      return 1;
    case "refused":
      return 2;
    case "sending":
      return 3;
    default:
      return 4;
  }
}

/** Deterministic sort mirroring Rust `import_store::build_document`. */
export function sortImportRows<T extends { id: string; state: string; added?: number }>(
  rows: readonly T[],
): T[] {
  return [...rows].sort((a, b) => {
    const rankDiff = importRowRank(a.state) - importRowRank(b.state);
    if (rankDiff !== 0) return rankDiff;
    const addedDiff = (b.added ?? 0) - (a.added ?? 0);
    if (addedDiff !== 0) return addedDiff;
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  });
}
