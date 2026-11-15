// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { t, stateWord, applyDocumentLocale, formatLocale, number } from "./i18n";
// The webview is a pure renderer. It subscribes to `health://changed` and paints
// the honest state it receives; it has no other input and cannot mint status.
// AutomationIds are stamped from the generated contract (see ./lib/contract.ts).

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import lockupSvg from "./brand/lockup-solstone-horizontal.svg?raw";
import "./brand/sunarc.js";
import { automationContract } from "./lib/contract";
import { createLogHelp } from "./logs";
import { displayableMark, MARK_CSS, renderMarkCard, type MarkRenderSpec } from "./mark";
import setupFrameCss from "./setup-frame.css?inline";
import shellCss from "./shell/shell.css?inline";
import { publishCaptionColor, resetCaptionReport } from "./shell/caption";
import { glyphForTile, type GlyphName } from "./shell/glyphs";
import {
  aboutJournalValue,
  addMoreSources,
  audioSubline,
  sourceSubline,
  audioTile,
  browsersTile,
  quantity,
  counterFacts,
  deckTiles,
  derivedComputerSoundWanted,
  durationTotal,
  greeting,
  journalFacts,
  journalPill,
  legacyRoute,
  obsidianTile,
  pauseTimeLeft,
  relativeTaken,
  restartPlace,
  screenTile,
  settingsNavigate,
  shelfBadges,
  spaceOnTile,
  statusLead,
  startedLabel,
  widthClass,
  type CounterDocument,
  type JournalPillInput,
  type Pane,
  type ShelfRow,
  type SourceFacts,
  type SourceId, screenshotsTile, screenshotsPage, screenshotsConfirm, type ScreenshotsStateName,
} from "./shell/derive";
import { renderDesk, type DeskStatusRow, type DeskTile } from "./shell/view";

type FrontendOrigin = "settings" | "about" | "none";
type FrontendErrorKind = "error" | "unhandled_rejection";

interface FrontendErrorRecord {
  kind: FrontendErrorKind;
  level: "error";
  origin: FrontendOrigin;
  line: number;
  column: number;
}

function installFrontendErrorHandlers(): void {
  try {
    window.addEventListener("error", (event: ErrorEvent) => {
      try {
        forwardFrontendError({
          kind: "error",
          level: "error",
          origin: resolveFrontendOrigin(),
          line: event.lineno || 0,
          column: event.colno || 0,
        });
      } catch {
        // Never let the error handler recurse.
      }
    });
    window.addEventListener("unhandledrejection", (_event: PromiseRejectionEvent) => {
      try {
        forwardFrontendError({
          kind: "unhandled_rejection",
          level: "error",
          origin: resolveFrontendOrigin(),
          line: 0,
          column: 0,
        });
      } catch {
        // Never let the error handler recurse.
      }
    });
  } catch {
    // Frontend error capture must be best-effort.
  }
}

function resolveFrontendOrigin(): FrontendOrigin {
  try {
    const label = getCurrentWindow().label;
    if (label === "settings" || label === "about") {
      return label;
    }
  } catch {
    // Tauri API unavailable during early module evaluation.
  }
  return "none";
}

function forwardFrontendError(record: FrontendErrorRecord): void {
  try {
    void invoke("log_frontend_error", { record }).catch(() => {});
  } catch {
    // Fire-and-forget only.
  }
}

type AppPhase = "idle" | "starting" | "observing" | "paused" | "error";
type SourceKind = "screen" | "system_audio" | "mic" | "desktop_activity";
type SourceStatus = "active" | "inactive" | "no_input_device" | "faulted";
type Severity = "ok" | "neutral" | "attention";
type Route = "home" | "sources" | "privacy" | "journal" | "shortcut" | "storage" | "updates" | "help";

interface SourceReport {
  kind: SourceKind;
  status: SourceStatus;
  reason?: string;
  detail?: string;
  device?: string | null;
}

type PairingPhase = "not_paired" | "pairing" | "awaiting_confirmation" | "paired" | "failed";

interface PairingState {
  phase: PairingPhase;
  journal_label: string | null;
  detail: string | null;
  mark?: MarkRenderSpec | null;
  binding?: string;
  finish_connecting?: boolean;
  setup_browser_ready?: boolean;
}

type MigrationChoice = "new_device" | "replace_device";
type MigrationPhase = "offered" | "decision_unknown" | "decided";
type MigrationUiIssue =
  | "offline"
  | "unsupported"
  | "storage_unavailable"
  | "decision_refused"
  | "target_missing"
  | "key_refused";

interface MigrationView {
  phase: MigrationPhase;
  revision: number;
  pairing_generation: number[];
  state: "none" | "pending" | "new_device" | "same_device" | "replaced_device" | null;
  replaced_cid: string | null;
  decision_choice: MigrationChoice | null;
  decision_result: string | null;
  offer_available: boolean;
  offer_binding: string | null;
}

interface MigrationSnapshot {
  migration: MigrationView | null;
  issue: MigrationUiIssue | null;
}

interface MigrationDevice {
  cid: string;
  display_label: string;
}

type MigrationFlow = "replace_offer" | "picker" | "confirm" | null;

const MIGRATION_COPY = {
  "migration.replace_offer.title": t("windows.desktop.shared.copy.replaceoffertitle"),
  "migration.replace_offer.body":
    t("windows.desktop.shared.copy.replaceofferbody"),
  "migration.replace_offer.pick": t("windows.desktop.shared.copy.choosedevice"),
  "migration.replace_offer.keep": t("windows.desktop.shared.copy.keepboth"),
  "migration.replace_offer.defer": t("windows.desktop.shared.copy.deferchoice"),
  "migration.picker.title": t("windows.desktop.shared.copy.choosedevice"),
  "migration.picker.empty": t("windows.desktop.shared.copy.pickerempty"),
  "migration.picker.cancel": t("windows.desktop.shared.setup.cancel"),
  "migration.replace_confirm.title": "",
  "migration.replace_confirm.body":
    t("windows.desktop.shared.copy.replaceconfirmbody"),
  "migration.replace_confirm.replace": t("windows.desktop.shared.copy.replacedevice"),
  "migration.replace_confirm.cancel": t("windows.desktop.shared.setup.cancel"),
  "migration.pending.row": t("windows.ui.devicechoice"),
  "migration.pending.value": t("windows.ui.notanswered"),
  "migration.preparing.title": t("windows.ui.gettingthisdeviceready"),
  "migration.offline.title": t("windows.desktop.shared.app.offlinetitle"),
  "migration.offline.body":
    t("windows.desktop.shared.copy.offlinebody"),
  "migration.offline.action": t("windows.desktop.shared.copy.tryagain"),
  "migration.unsupported.title": t("windows.ui.journalupdateneeded"),
  "migration.unsupported.body":
    t("windows.ui.yourjournaldoesntsupportthismove"),
  "migration.unsupported.action": t("windows.desktop.shared.copy.tryagain"),
  "migration.storage_unavailable.title": t("windows.desktop.shared.copy.storageunavailabletitle"),
  "migration.storage_unavailable.body":
    t("windows.desktop.shared.copy.storageunavailablebody"),
  "migration.storage_unavailable.action": t("windows.desktop.shared.copy.technicaldetails"),
  "migration.deciding.title": t("windows.desktop.shared.copy.decidingtitle"),
  "migration.decision_unknown.title": t("windows.desktop.shared.copy.decisionunknowntitle"),
  "migration.decision_unknown.body": t("windows.desktop.shared.copy.decisionunknownbody"),
  "migration.decision_unknown.action": t("windows.desktop.shared.copy.checkagain"),
  "migration.list_unavailable.title": t("windows.desktop.shared.copy.listunavailabletitle"),
  "migration.list_unavailable.body":
    t("windows.desktop.shared.copy.listunavailablebody"),
  "migration.list_unavailable.action": t("windows.desktop.shared.copy.tryagain"),
  "migration.target_missing.title": t("windows.desktop.shared.copy.targetmissingtitle"),
  "migration.target_missing.body":
    t("windows.desktop.shared.copy.targetmissingbody"),
  "migration.target_missing.action": t("windows.desktop.shared.copy.choosedevice"),
  "migration.decision_refused.title": t("windows.desktop.shared.copy.decisionrefusedtitle"),
  "migration.decision_refused.body":
    t("windows.desktop.shared.copy.decisionrefusedbody"),
  "migration.decision_refused.action": t("windows.desktop.shared.copy.technicaldetails"),
  "migration.key_refused.title": t("windows.ui.pairagain"),
  "migration.key_refused.body":
    t("windows.ui.thisdevicecouldntopenitssaved"),
  "migration.key_refused.action": t("windows.ui.pairagain"),
  "migration.replace_confirm.title_fallback": t("windows.desktop.shared.copy.replaceconfirmtitlefallback"),
} as const;

interface UploadStatus {
  pending_segments: number;
  uploaded_segments: number;
  failed_segments: number;
  quarantined_segments?: number;
  last_uploaded_segment: string | null;
  last_error: string | null;
  last_successful_sync?: number;
  recent_error_count?: number;
  last_error_reason?: string | null;
}

interface UnknownJournalSighting {
  address: string | null;
  expected_mark: MarkRenderSpec;
  responding_mark: MarkRenderSpec | null;
}

interface SyncSnapshot {
  pairing: PairingState;
  upload: UploadStatus;
  imports_waiting?: number;
  imports_need_owner?: boolean;
  journal_version?: string | null;
  journal_version_fresh?: boolean;
  journal_display_line?: string;
  journal_base_line?: string;
  journal_seen_at_epoch_secs?: number | null;
  about_app_line?: string;
  about_block?: string;
  unknown_journals?: UnknownJournalSighting[];
  // The app's one presented journal connection, shared with the tray.
  journal_link?: "connected" | "connecting" | "offline" | null;
}

interface EncoderHealth {
  frames_consumed: number;
  samples_written: number;
  last_error: string | null;
}

interface ExclusionHealth {
  rules_active: boolean;
  frames_redacted: number;
  frames_dropped: number;
}

interface PauseSnapshot {
  reason: "operator" | "session_locked" | "system_suspending";
  seconds_remaining: number | null;
}

type WizardStep = "mic" | "system_audio" | "screen" | "connection" | "done";

interface FailedSourceWrite {
  kind: SourceKind;
  attempted: boolean;
}

interface SetupSnapshot {
  sequence_enabled?: boolean;
  mic: boolean;
  system_audio: boolean;
  screen: boolean;
  start_at_login: boolean | null;
  wizard: WizardStep;
  notice_dismissed: boolean;
  desktop_activity?: boolean;
  desktop_activity_allowed?: boolean;
  desktop_activity_through_pause?: boolean;
  desktop_activity_window_titles?: boolean;
  desktop_activity_page_addresses?: boolean;
  system_audio_wanted?: boolean | null;
  desktop_activity_through_pause_save_failed?: boolean;
  desktop_activity_window_titles_save_failed?: boolean;
  desktop_activity_page_addresses_save_failed?: boolean;
  failed_sources?: FailedSourceWrite[];
  wizard_save_failed?: boolean;
  pending_wizard?: WizardStep | null;
  notice_save_failed?: boolean;
  login_apply_failed?: boolean;
  setup_read_failed?: boolean;
  handoff_id?: string | null;
  handoff_revision?: number | null;
  handoff_expired?: boolean;
  handoff_new_connection?: boolean;
  handoff_deferred?: boolean;
  mailbox_busy?: boolean;
  handoff_rejected?: boolean;
  native_handoff_rejected?: boolean;
  handoff_persist_failed?: boolean;
  has_deferred_link?: boolean;
}

interface ScreenPreviewFrame {
  width: number;
  height: number;
  rgba: number[];
}

interface PreviewSnapshot {
  consumer_generation?: number;
  mic_level?: number | null;
  system_audio_level?: number | null;
  screen_frame?: ScreenPreviewFrame | null;
}

function isSourceEnabled(setup: SetupSnapshot | undefined, kind: SourceKind): boolean {
  if (!setup) {
    return kind === "desktop_activity" ? false : true;
  }
  switch (kind) {
    case "screen":
      return setup.screen;
    case "system_audio":
      return setup.system_audio;
    case "mic":
      return setup.mic;
    case "desktop_activity":
      return Boolean(setup.desktop_activity);
  }
}

// Desktop activity can be kept on through the main pause; every other source pauses.
function isSourcePaused(dump: HealthDump, kind: SourceKind): boolean {
  const paused = dump.pause !== null || dump.app_state === "paused";
  return paused && !(kind === "desktop_activity" && dump.setup?.desktop_activity_through_pause === true);
}

function deriveSourceStatus(
  enabled: boolean,
  isPaused: boolean,
  report: SourceReport | undefined,
): "off" | "paused" | "on" | "unavailable" | "error" | null {
  if (!enabled) {
    return "off";
  }
  if (isPaused) {
    return "paused";
  }
  if (!report) {
    return null;
  }
  if (report.status === "active") {
    return "on";
  }
  if (
    report.status === "no_input_device" ||
    (report.status === "faulted" && (report.reason === "access_denied" || report.reason === "AccessDenied"))
  ) {
    return "unavailable";
  }
  if (report.status === "faulted") {
    return "error";
  }
  return null;
}

interface HealthDump {
  app_state: AppPhase;
  sources: SourceReport[];
  frame_rate: number | null;
  segment_dir: string | null;
  segment_seconds_remaining: number | null;
  engine_ready: boolean;
  version: string;
  sync: SyncSnapshot;
  screen_encoder: EncoderHealth | null;
  exclusions: ExclusionHealth | null;
  pause: PauseSnapshot | null;
  views?: Record<string, string>;
  setup?: SetupSnapshot;
}

interface StorageInfo {
  root: string;
  bytes: number | null;
}

// ── Capture exclusions (observer-exclusion) ──────────────────────────────────
// The owner's privacy controls. Rules are loaded once and edited in place; each
// edit calls `set_exclusions` (effective on the next captured frame, persisted)
// and re-renders. Exclusion *activity* is read from the health dump so the
// owner can see exclusions working — it is never silent.

interface ExclusionRules {
  excluded_exes: string[];
  title_patterns: string[];
  exclude_private_browsing: boolean;
}

interface RunningApp {
  exe_name: string;
  display_name: string;
}

interface SetExclusionsOutcome {
  persisted: boolean;
}

type ExclusionEdit =
  | { kind: "toggle" }
  | { kind: "appAdd" }
  | { kind: "titleAdd" }
  | { kind: "appRemove"; value: string }
  | { kind: "titleRemove"; value: string };

const SELF_EXE = "solstone-windows-app.exe";
const STORE_APPS_LABEL =
  t("windows.ui.storeappsallappsfromthemicrosoft");

// ── Updater (observer-update) ────────────────────────────────────────────────
// Update state arrives on its own `update://changed` event, separate from the
// health stream, and is rendered into the Updates section honestly: every control
// is enabled only when its `actions` flag is true (no dead buttons), and the
// state text never claims "up to date" without an earned up-to-date result.

type UpdateDisplayKind =
  | "never_checked"
  | "up_to_date"
  | "checking"
  | "available"
  | "downloading"
  | "staged"
  | "failed"
  | "failed_with_available"
  | "unavailable";
type UpdateActivityKind = "idle" | "checking" | "downloading" | "installing";
type CheckIntervalKind = "day" | "week" | "month";

interface UpdatePrefs {
  auto_check: boolean;
  interval: CheckIntervalKind;
  auto_download: boolean;
}

interface UpdateActions {
  can_check_now: boolean;
  can_cancel: boolean;
  can_download: boolean;
  can_install: boolean;
  can_retry: boolean;
  can_dismiss: boolean;
  frequency_enabled: boolean;
}

interface UpdateView {
  display: UpdateDisplayKind;
  activity: UpdateActivityKind;
  last_checked_at: number | null;
  available_version: string | null;
  downloaded_version: string | null;
  downloaded: boolean;
  cost_deferral: "metered" | "roaming" | "restricted" | "unknown" | null;
  installs_next_start: boolean;
  recovery: "check" | "download" | "install" | null;
  notes: string | null;
  download_pct: number | null;
  prefs: UpdatePrefs;
  actions: UpdateActions;
}

// Latest snapshots; the settings view renders from both. Held in module vars so a
// re-render from either stream never loses the other's state.
let latestHealth: HealthDump | null = null;
let aboutClipboardWriter: ((value: string) => Promise<void>) | null = null;
let latestStorage: StorageInfo | null = null;
let latestUpdate: UpdateView | null = null;
let latestMigration: MigrationSnapshot | null = null;
let latestMigrationDevices: MigrationDevice[] | null = null;
let migrationFlow: MigrationFlow = null;
let migrationBusy = false;
let migrationDevicesBusy = false;
let migrationListUnavailable = false;
let migrationShowDetails = false;
let migrationPromptHidden = false;
let migrationRefreshAt = 0;
let migrationRefreshInFlight = false;
let migrationOfferBinding: string | null = null;
let selectedMigrationDevice: MigrationDevice | null = null;
let activeRoute: Route = "home";
let deskRow: ShelfRow = "home";
let deskPane: Pane | "addMore" | "technical" = "status";
let deskShelfOpen = false;
let deskPhonePane = false;
let latestCounters: CounterDocument | null | undefined;
let countersFor: HealthDump | null = null;
let captionWatch: MutationObserver | null = null;
let deskKeysBound = false;
// Where the window was asked to open (`?route=` at creation, or a navigation that
// arrived before the first health). It is applied once setup is known: setup that is
// not done shows setup, and a finished setup opens the deck or that source's page.
let startupDestination: string | null = null;
let renderBeaconFired = false;
let focusPaneTitleOnRender = false;
type ProofConsumerState = "mic" | "system_audio" | "screen" | "closed";
let openProofKind: ProofConsumerState = "closed";
let currentAcceptedProofGeneration = 0;
let proofRequest = 0;
let proofWindowActive = true;
let proofOperations: Promise<unknown> = Promise.resolve();
let focusWizardPrimary = false;
let markCardFocused = false;
// Set when a background event (health/update stream) wants a full rerender but an
// interactive control is active; the next flush or any direct rerender consumes it,
// coalescing many deferred events into a single repaint.
let pendingRerender = false;
// The last-checked line node, refreshed each render; a 1s interval rewrites its
// text so the relative clock ticks live — the JS analog of the macOS TimelineView.
let lastCheckedEl: HTMLElement | null = null;
// Bounded-pause status pills currently mounted (home route mounts two: the
// status strip + the pause card). The 1 s timer advances their "X min left"
// text without a rerender — (c2) no longer re-emits the per-second countdown.
let pauseStatusEls: HTMLElement[] = [];
// Wall-clock (epoch secs) when the current `latestHealth` was received; the
// anchor for the display-only pause countdown.
let healthReceivedAt: number | null = null;
// The status strip's next-save countdown and last-synced age, refreshed each
// render and advanced by the same 1 s timer without a rerender.
let nextSaveEl: HTMLElement | null = null;
let lastSyncedEl: HTMLElement | null = null;

// The pair-link the owner typed/pasted. Held in a module var (never read back
// from the DOM) so a health re-render never loses it.
let pairingDraft = "";
let pairFieldDirty = false;
let pairingBusy = false;
let finishInFlight = false;
let browserInFlight = false;
let retryInFlight = false;
let nativeConnectInFlight = false;
let deferredInFlight = false;
const setupActionError = { finish: "", browser: "", retry: "", connect: "", deferred: "" };
// Setup pause and unknown-login failures survive a health rerender. They clear
// on a successful retry, or when the dump shows the pause or login choice landed.
let wizardPauseFailure: "pause" | "resume" | null = null;
let unknownLoginAttempt: boolean | null = null;

// A journal the app found on this computer, while it holds no pairing. Only ever
// an offer: nothing connects until the owner presses its one button.
interface FoundJournal {
  instance_id: string;
  mark: MarkRenderSpec;
}
const SAME_COMPUTER_UNVERIFIED = "same_computer_unverified";
const FOUND_JOURNAL_LOOK_MS = 3000;
let foundJournal: FoundJournal | null = null;
let foundJournalLookInFlight = false;
let foundJournalLookedAt = 0;
let foundJournalConnecting = false;
// A look still in flight when the owner presses answers for the moment before the
// press, so its answer is dropped rather than pulling the offer out from under it.
let foundJournalPresses = 0;
// The press that connected, for this window's life: says "connected to" rather
// than the ordinary paired status, while the journal link presents as connected.
let foundJournalConnected = false;
// The press verified the journal over the pinned connection, which is hearing from
// it. With no source on nothing presents the link, so that answer carries the row
// for one mains lease: lease_ms(FOREGROUND_CHECK_SECS) in observer-model's
// journal_link.rs, 15 s + 5 s + 1 s.
const FOUND_JOURNAL_PRESS_LEASE_MS = 21_000;
let foundJournalConnectedAt: number | null = null;
let pairingStatusEl: HTMLElement | null = null;

const SETUP_LINK_ERRORS: Record<string, string> = {
  "no credential access": t("windows.ui.nocredentialaccess"),
  "setup connection changed": t("windows.ui.setupconnectionchanged"),
  "not paired": t("windows.desktop.shared.app.notpaired"),
  "setup unavailable": t("windows.ui.setupunavailable"),
  "source setup is unfinished": t("windows.ui.sourcesetupisunfinished"),
  "browser state unavailable": t("windows.ui.browserstateunavailable"),
  "setup progress could not be saved": t("windows.ui.setupprogresscouldnotbesaved"),
  "no local journal to connect to": t("windows.ui.nolocaljournaltoconnectto"),
  "failed to acquire setup lock": t("windows.ui.failedtoacquiresetuplock"),
  "setup sequence is unavailable in this build": t("windows.ui.setupsequenceisunavailableinthisbuild"),
  setup_connect_failed: t("windows.ui.thisconnectioncouldnotfinishtryagain"),
  setup_finish_failed: t("windows.ui.thisconnectioncouldnotfinishtryagain2"),
  setup_browser_failed: t("windows.ui.thisstepcouldnotfinishtryagain"),
  setup_browser_completion_failed: t("windows.ui.yourjournalopenedbutsetupprogresscould"),
  setup_link_use_failed: t("windows.ui.thislinkcouldnotbeusedtry"),
  setup_link_dismiss_failed: t("windows.ui.thislinkcouldnotbedismissedtry"),
  handoff_persist_failed: t("windows.ui.thisconnectioncouldnotbesavedtry"),
  handoff_busy: t("windows.ui.setupisbusytryagainina"),
  handoff_rejected: t("windows.ui.thissetuplinkcouldnotbeused"),
  handoff_deferred: t("windows.ui.finishthecurrentconnectionthentryagain"),
};

function errorText(error: unknown): string {
  if (error instanceof Error && error.message) {
    return SETUP_LINK_ERRORS[error.message] ?? error.message;
  }
  if (typeof error === "string" && error) {
    return SETUP_LINK_ERRORS[error] ?? error;
  }
  return t("windows.ui.thatdidnotfinish");
}

function appendSetupError(
  parent: HTMLElement,
  kind: "finish" | "browser" | "retry" | "connect" | "deferred",
): void {
  const message = setupActionError[kind];
  if (!message) {
    return;
  }
  const node = document.createElement("div");
  node.dataset.setupError = kind;
  node.textContent = message;
  parent.append(node);
}

function nativeContinuationPending(dump: HealthDump): boolean {
  if (dump.setup?.sequence_enabled !== true) return false;
  const setup = dump.setup;
  const pairing = dump.sync.pairing;
  if (pairing.setup_browser_ready || pairing.finish_connecting) {
    return true;
  }
  return Boolean(
    setup?.handoff_id && !setup.handoff_expired &&
      (pairing.phase !== "paired" || setup.handoff_new_connection === true),
  );
}

// A pairing link that opens the app while it is unpaired and setup is finished
// waits in the journal pane's field, and home has no field. The window's first
// render, and each time it comes forward with an untouched field, opens the
// journal pane so the waiting link is in view.
let waitingLinkChecked = false;

function openJournalPaneForWaitingLink(dump: HealthDump): void {
  if (waitingLinkChecked) return;
  waitingLinkChecked = true;
  if (label !== "settings" || activeRoute !== "home") return;
  // Coming back to a held pairing lands on the panel that answers it.
  if (
    dump.sync.pairing.phase === "awaiting_confirmation" &&
    !settingsShowsSetupFrame(dump) &&
    !homeShowsPairing(dump)
  ) {
    navigateTo("journal");
    return;
  }
  if (dump.setup?.sequence_enabled !== true || dump.setup.wizard !== "done") return;
  if (dump.sync.pairing.phase !== "not_paired" || homeShowsPairing(dump)) return;
  void Promise.resolve(invoke<string>("pairing_prefill"))
    .then((link) => {
      if (typeof link !== "string" || !link) return;
      if (activeRoute !== "home" || latestHealth?.sync.pairing.phase !== "not_paired") return;
      navigateTo("journal");
    })
    .catch(() => {});
}

function foundJournalLookApplies(dump: HealthDump): boolean {
  if (label !== "settings" || document.visibilityState === "hidden") return false;
  const phase = dump.sync.pairing.phase;
  if (phase !== "not_paired" && phase !== "failed") return false;
  if (nativeContinuationPending(dump)) return false;
  return settingsShowsSetupFrame(dump)
    ? dump.setup?.wizard === "connection"
    : activeRoute === "journal";
}

// Runs on the window's one-second clock. It costs one local lookup every few
// seconds, and only while the connect step or journal pane is showing with no
// pairing held, so a journal that finishes setup later is offered with no restart.
function lookForFoundJournal(): void {
  const dump = latestHealth;
  if (!dump || foundJournalConnecting || pairingBusy || !foundJournalLookApplies(dump)) {
    if (foundJournal && !foundJournalConnecting) {
      foundJournal = null;
      requestRerender();
    }
    return;
  }
  const now = Date.now();
  if (foundJournalLookInFlight || now - foundJournalLookedAt < FOUND_JOURNAL_LOOK_MS) return;
  foundJournalLookInFlight = true;
  foundJournalLookedAt = now;
  const presses = foundJournalPresses;
  void Promise.resolve(invoke<FoundJournal | null>("find_local_journal"))
    .catch(() => null)
    .then((found) => {
      foundJournalLookInFlight = false;
      if (presses !== foundJournalPresses) return;
      const next = found && displayableMark(found.mark) ? found : null;
      if (next?.instance_id === foundJournal?.instance_id) return;
      foundJournal = next;
      requestRerender();
    });
}

async function connectFoundJournal(found: FoundJournal): Promise<void> {
  if (foundJournalConnecting || pairingBusy) return;
  foundJournalConnecting = true;
  foundJournalPresses += 1;
  rerender();
  try {
    await invoke("connect_found_journal", { instanceId: found.instance_id });
    foundJournalConnected = true;
    foundJournalConnectedAt = Date.now();
    foundJournal = null;
  } catch {
    // Nothing connected; the pairing status carries the failure to the next render.
  } finally {
    foundJournalConnecting = false;
    retryHealth();
  }
}

function foundJournalPressHolds(): boolean {
  if (foundJournalConnectedAt === null) return false;
  const elapsed = Date.now() - foundJournalConnectedAt;
  return elapsed >= 0 && elapsed < FOUND_JOURNAL_PRESS_LEASE_MS;
}

function pairingStatusLabel(dump: HealthDump): string {
  const presented = presentedPairing(dump.sync.pairing);
  if (foundJournalConnected && presented.phase === "paired") {
    const link = dump.sync.journal_link ?? null;
    if (link === "connected" || (link === null && foundJournalPressHolds())) {
      return t("windows.ui.connectedto", { "value": String(markTextForm(presented.mark)) });
    }
  }
  return pairingPhaseLabel(presented);
}

// Runs on the window's one-second clock: a lapse changes nothing else, so no health
// update would repaint the row, and a focused paste field holds back a full render.
function lapseFoundJournalPress(): void {
  if (foundJournalConnectedAt === null || foundJournalPressHolds()) return;
  foundJournalConnectedAt = null;
  if (pairingStatusEl?.isConnected && latestHealth) {
    pairingStatusEl.textContent = pairingStatusLabel(latestHealth);
  }
}

function renderFoundJournal(found: FoundJournal): HTMLElement {
  const panel = automation(document.createElement("div"), ids["settings.pairing.foundJournal"]);
  panel.style.display = "flex";
  panel.style.flexDirection = "column";
  panel.style.alignItems = "flex-start";
  panel.style.gap = "10px";
  panel.style.padding = "7px 0 12px";
  const heading = text("div", t("windows.ui.foundjournalpc"));
  heading.style.fontWeight = "600";
  heading.style.fontSize = "14px";
  const connect = actionButton(
    foundJournalConnecting ? t("windows.ui.connecting") : t("windows.ui.connecttoyourjournal"),
    ids["settings.pairing.foundConnect"],
    !foundJournalConnecting,
    () => void connectFoundJournal(found),
    foundJournalConnecting,
    "accent",
  );
  connect.classList.add("setup-primary");
  panel.append(heading, renderMarkCard(found.mark, "unavailable"), connect);
  return panel;
}

function homeShowsPairing(dump: HealthDump): boolean {
  if (dump.setup?.sequence_enabled !== true) return false;
  const setup = dump.setup;
  const pairing = dump.sync.pairing;
  return Boolean(
    setup?.handoff_id ||
      setup?.handoff_deferred ||
      setup?.mailbox_busy ||
      setup?.handoff_rejected ||
      setup?.native_handoff_rejected ||
      setup?.handoff_persist_failed ||
      setup?.has_deferred_link ||
      pairing.finish_connecting ||
      pairing.setup_browser_ready,
  );
}
let journalOpenError = false;

type MigrationCopyKey = keyof typeof MIGRATION_COPY;

function migrationCopy(key: MigrationCopyKey, values: Record<string, string> = {}): string {
  if (key === "migration.replace_confirm.title") {
    return t("windows.desktop.shared.copy.replaceconfirmtitle", { selecteddevicelabel: values.selected_device_label ?? "" });
  }
  return MIGRATION_COPY[key].replace(/\{([^}]+)\}/g, (_match, name: string) => values[name] ?? "");
}

function migrationText<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  key: MigrationCopyKey,
  values: Record<string, string> = {},
): HTMLElementTagNameMap[K] {
  const node = text(tag, migrationCopy(key, values)) as HTMLElementTagNameMap[K];
  node.dataset.migrationKey = key;
  return node;
}

function migrationAction(key: MigrationCopyKey, onClick: () => void): HTMLButtonElement {
  const button = actionButton(migrationCopy(key), undefined, true, onClick, migrationBusy);
  button.dataset.migrationKey = key;
  button.dataset.migrationAction = key;
  return button;
}

async function refreshMigrationState(force = false): Promise<void> {
  if (migrationRefreshInFlight) return;
  if (!force && Date.now() - migrationRefreshAt < 5000) return;
  migrationRefreshInFlight = true;
  migrationRefreshAt = Date.now();
  try {
    latestMigration = await invoke<MigrationSnapshot>("pairing_migration_state");
    const nextBinding = latestMigration.migration?.offer_binding ?? null;
    if (nextBinding !== migrationOfferBinding) {
      migrationOfferBinding = nextBinding;
      migrationPromptHidden = false;
    }
  } catch {
    latestMigration = {
      migration: null,
      issue: "storage_unavailable",
    };
  } finally {
    migrationRefreshInFlight = false;
    requestRerender();
  }
}

async function loadMigrationDevices(): Promise<void> {
  if (migrationDevicesBusy) return;
  migrationDevicesBusy = true;
  migrationListUnavailable = false;
  try {
    latestMigrationDevices = await invoke<MigrationDevice[]>("pairing_migration_devices");
  } catch {
    latestMigrationDevices = null;
    migrationListUnavailable = true;
  } finally {
    migrationDevicesBusy = false;
    requestRerender();
  }
}

async function submitMigrationDecision(
  choice: MigrationChoice,
  replacesCid: string | null = null,
): Promise<void> {
  if (migrationBusy) return;
  migrationBusy = true;
  migrationFlow = null;
  latestMigration = {
    migration: latestMigration?.migration ?? null,
    issue: null,
  };
  requestRerender();
  try {
    latestMigration = await invoke<MigrationSnapshot>("pairing_migration_decide", {
      choice,
      replacesCid,
    });
    migrationOfferBinding = latestMigration.migration?.offer_binding ?? migrationOfferBinding;
  } catch {
    latestMigration = {
      migration: latestMigration?.migration ?? null,
      issue: "decision_refused",
    };
  } finally {
    migrationBusy = false;
    requestRerender();
  }
}

async function dismissMigrationOffer(): Promise<void> {
  const migration = latestMigration?.migration;
  if (
    migrationBusy ||
    !migration?.offer_binding ||
    !migration.offer_available
  ) {
    return;
  }
  migrationBusy = true;
  try {
    await invoke("pairing_migration_offer_dismiss", {
      expectedBinding: migration.offer_binding,
      expectedGeneration: migration.pairing_generation,
      expectedRevision: migration.revision,
    });
    migrationPromptHidden = true;
    migrationFlow = null;
    await refreshMigrationState(true);
  } catch {
    latestMigration = { migration, issue: "storage_unavailable" };
  } finally {
    migrationBusy = false;
    requestRerender();
  }
}

// Capture-exclusion rules + the running-app picker list. Held in module vars so a
// 1s health re-render repaints the section without losing edits; `titleDraft`
// preserves a half-typed title keyword across re-renders (like pairingDraft).
let latestExclusions: ExclusionRules | null = null;
let latestHotkey: HotkeyView | null = null;
let exclusionsPending: ExclusionEdit | null = null;
let exclusionsPersisted = true;
// True while the press-to-capture field is listening for the owner's next combo.
// Held in a module var so a 1s health re-render keeps the capturing state; the
// keydown listener lives on the window, added once when capture starts.
let hotkeyCapturing = false;
let latestMic: MicView | null = null;
let latestPreview: PreviewSnapshot = {};

// The browser extension's path, present only in a build with the browser host
// (the backend answers `null` otherwise, and nothing here renders). States and
// counts only: the app never hands page text to the webview.
type BrowserStatus = {
  capture: "unavailable" | "not_paired" | "permitted" | "paused" | "intake_off";
  delivery: "unknown" | "kept_locally" | "delivered" | "idle" | "failed";
  failure: string | null;
  connected: { brand: string; connected_at_ms: number }[];
  custody: { held_bytes: number; waiting: boolean; full: boolean };
  stores: string[];
};
let latestBrowser: BrowserStatus | null = null;
let browserDiscard: "idle" | "confirm" | "busy" | "done" | "failed" = "idle";
let browserStoreFailure: string | null = null;
type ObsidianStateName =
  | "off"
  | "checking"
  | "confirm"
  | "on"
  | "paused"
  | "waiting_for_journal_update"
  | "needs_attention";
type ObsidianAttention = "vault_unreadable" | "part_unreadable" | "file_refused" | "storage";
type ObsidianRefusal = "not_a_vault" | "unusable_name";
type ObsidianStatus = {
  state: ObsidianStateName;
  vault: string | null;
  warning: { vault: string; device: string | null; last_synced_at: string | null } | null;
  last_checked_ms: number | null;
  waiting: number;
  first_sync: { found: number; sent: number } | null;
  not_on_device: number;
  too_large: number;
  attention: ObsidianAttention | null;
};
type ObsidianDocument = { v: number; status: ObsidianStatus; refusal?: ObsidianRefusal | null };
type ObsidianVault = { name: string; path: string };
let latestObsidian: ObsidianStatus | null = null;
type ScreenshotsSpan = "thirty_days" | "six_months" | "year" | "all" | "nothing";
type ScreenshotsStatus = {
  state: ScreenshotsStateName;
  folder: string | null;
  folder_line: string | null;
  folder_name: string | null;
  windows_folder: boolean;
  counting: boolean;
  count_failed: boolean;
  counts: Partial<Record<ScreenshotsSpan, number>> | null;
  span: ScreenshotsSpan;
  turned_back_on: boolean;
  backlog: { done: number; total: number } | null;
  waiting_for_journal: boolean;
  waiting: number;
  cant_come_in: number;
  unreadable: number;
  last_checked_ms: number | null;
};
type ScreenshotsDocument = { v: number; status: ScreenshotsStatus };
let latestScreenshots: ScreenshotsStatus | null = null;
let screenshotsBusy = false;
/** The span the owner picked in the chooser; the first choice is preselected. */
let screenshotsSpan: ScreenshotsSpan = "thirty_days";

interface ImportRow {
  id: string;
  name: string;
  state: "waiting" | "sending" | "refused";
  reason: string | null;
  retryable: boolean;
  may_have_reached: boolean;
  missing_copy: boolean;
}
interface ImportDocument {
  recent: string;
  rows: ImportRow[];
  intake: { copying: boolean; done: number; total: number; accepted: number; refused: { name: string; reason: string }[] } | null;
  interrupted: boolean;
  waiting: number;
  needs_owner: boolean;
  available: boolean;
}
let latestImport: ImportDocument | null = null;
let importActionError: string | null = null;

let obsidianFound: ObsidianVault[] = [];
let obsidianRefusal: ObsidianRefusal | null = null;
let obsidianBusy = false;
let obsidianWantsFolder = false;
let obsidianFailed = false;
let micDevices: MicDeviceRef[] = [];
let runningApps: RunningApp[] = [];
let titleDraft = "";

const ids = automationContract.automation_ids;
let root!: HTMLDivElement;
let narrowNav!: MediaQueryList;
let label = "settings";

const ROUTES: ReadonlyArray<{ route: Route; label: string; glyph: string }> = [
  { route: "home", label: t("windows.ui.home"), glyph: "\uE80F" },
  { route: "sources", label: t("windows.desktop.shared.app.sourcestitle"), glyph: "\uE8B3" },
  { route: "privacy", label: t("windows.ui.privacy"), glyph: "\uEA18" },
  { route: "journal", label: t("windows.desktop.shared.about.pendingsettingstab"), glyph: "\uE753" },
  { route: "shortcut", label: t("windows.ui.shortcut"), glyph: "\uE765" },
  { route: "storage", label: t("windows.ui.storage"), glyph: "\uE8B7" },
  { route: "updates", label: t("windows.desktop.shared.settings.updates"), glyph: "\uE777" },
  { route: "help", label: t("windows.desktop.shared.settings.help"), glyph: "\uE897" },
];

let logHelp: HTMLElement | null = null;

const NATIVE_FEEL_CSS = `
:root {
  color-scheme: light dark;
  --fg: rgba(0,0,0,0.886);
  --fg-subtle: rgba(0,0,0,0.60);
  --muted: rgba(0,0,0,0.45);
  --bg: rgba(249,249,249,0.80);
  --bg-input: #ffffff;
  --border: rgba(0,0,0,0.0803);
  --border-subtle: rgba(0,0,0,0.06);
  --accent: #0067c0;
  --accent-fg: #ffffff;
  --accent-busy: rgba(0,103,192,0.55);
  --accent-subtle: rgba(0,103,192,0.10);
  --danger: #c42b1c;
  --fill: rgba(0,0,0,0.045);
  --radius-control: 4px;
  --radius-card: 8px;
  --dur-fast: 83ms;
  --ease-standard: cubic-bezier(0,0,0,1);
  --overlay-hover: rgba(0,0,0,0.037);
	  --overlay-pressed: rgba(0,0,0,0.024);
	  --accent-overlay-hover: rgba(255,255,255,0.10);
	  --accent-overlay-pressed: rgba(0,0,0,0.06);
	  --rail-w: 200px;
	}

@media (prefers-color-scheme: dark) {
  :root {
    --fg: #ffffff;
    --fg-subtle: rgba(255,255,255,0.606);
    --muted: rgba(255,255,255,0.50);
    --bg: rgba(32,32,32,0.80);
    --bg-input: #2b2b2b;
    --border: rgba(255,255,255,0.10);
    --border-subtle: rgba(255,255,255,0.07);
    --accent: #60cdff;
    --accent-fg: #000000;
    --accent-busy: rgba(96,205,255,0.45);
    --accent-subtle: rgba(96,205,255,0.14);
    --danger: #ff99a4;
    --fill: rgba(255,255,255,0.065);
    --radius-control: 4px;
    --radius-card: 8px;
    --overlay-hover: rgba(255,255,255,0.045);
    --overlay-pressed: rgba(255,255,255,0.030);
  }
}

@supports (color: AccentColor) {
  :root {
    --accent: AccentColor;
    --accent-fg: AccentColorText;
  }
}

html,
body {
  margin: 0;
  height: 100%;
  overflow: hidden;
  overscroll-behavior: none;
  background: transparent;
}

body {
  font-family: "Segoe UI Variable Text", "Segoe UI Variable", "Segoe UI", system-ui, sans-serif;
  color: var(--fg);
  user-select: none;
  -webkit-user-select: none;
  cursor: default;
}

#app {
  height: 100vh;
  min-height: 100vh;
  overflow-y: auto;
  overscroll-behavior: none;
  background: var(--bg);
  color: var(--fg);
  scrollbar-width: thin;
}

button,
input,
textarea,
select {
  font: inherit;
}

input[type="checkbox"] {
  accent-color: var(--accent);
}

input,
textarea {
  user-select: text;
  -webkit-user-select: text;
  cursor: text;
}

.log-help p { font-size: 13px; line-height: 1.5; color: var(--fg-subtle); }
.log-help button {
  padding: 6px 12px;
  border: 1px solid var(--border);
  border-radius: var(--radius-control);
  background: var(--fill);
  color: var(--fg);
  cursor: pointer;
}
.log-help button:disabled { color: var(--muted); cursor: default; }
.log-help textarea {
  padding: 8px;
  border: 1px solid var(--border);
  border-radius: var(--radius-control);
  background: var(--surface);
  color: var(--fg);
  margin-bottom: 12px;
}

.fluent-control:not(:disabled):hover {
  box-shadow: inset 0 0 0 999px var(--overlay-hover);
}

.fluent-control:not(:disabled):active {
  box-shadow: inset 0 0 0 999px var(--overlay-pressed);
}

.fluent-accent:not(:disabled):hover {
  box-shadow: inset 0 0 0 999px var(--accent-overlay-hover);
}

.fluent-accent:not(:disabled):active {
  box-shadow: inset 0 0 0 999px var(--accent-overlay-pressed);
}

button:focus-visible,
input:focus-visible,
textarea:focus-visible,
select:focus-visible,
a:focus-visible,
[tabindex]:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

::selection {
  background: var(--accent);
  color: var(--accent-fg);
}

.selectable {
  user-select: text;
  -webkit-user-select: text;
  cursor: text;
}

	.scroll-surface {
	  scrollbar-width: thin;
	}

	.settings-shell {
	  position: relative;
	  display: grid;
	  grid-template-columns: var(--rail-w) minmax(0, 1fr);
	  height: 100%;
	  min-height: 0;
	  overflow: hidden;
	}

	.settings-scrim {
	  display: none;
	}

	.settings-rail {
	  min-height: 0;
	  overflow-y: auto;
	  border-right: 1px solid var(--border);
	  background: color-mix(in srgb, var(--bg) 92%, transparent);
	  padding: 14px 10px;
	  box-sizing: border-box;
	  scrollbar-width: thin;
	}

	.settings-rail-title {
	  padding: 4px 10px 14px;
	  font-size: 18px;
	  font-weight: 700;
	}

	.settings-nav-item {
	  position: relative;
	  display: flex;
	  align-items: center;
	  gap: 10px;
	  width: 100%;
	  min-height: 36px;
	  padding: 8px 10px;
	  border: 1px solid transparent;
	  border-radius: var(--radius-control);
	  background: transparent;
	  color: var(--fg);
	  font-size: 13px;
	  text-align: left;
	  cursor: pointer;
	}

	.settings-nav-item[aria-current="page"] {
	  background: var(--accent-subtle);
	}

	.settings-nav-item[aria-current="page"]::before {
	  content: "";
	  position: absolute;
	  left: 0;
	  top: 7px;
	  bottom: 7px;
	  width: 3px;
	  border-radius: 0 2px 2px 0;
	  background: var(--accent);
	}

	.settings-nav-glyph,
	.settings-hamburger-glyph {
	  font-family: "Segoe Fluent Icons", "Segoe MDL2 Assets", sans-serif;
	  font-size: 16px;
	  line-height: 1;
	}

	.settings-nav-glyph {
	  width: 20px;
	  flex: 0 0 20px;
	  text-align: center;
	}

	.settings-pane {
	  min-width: 0;
	  height: 100%;
	  overflow-y: auto;
	  scrollbar-width: thin;
	}

	.settings-pane-frame {
	  box-sizing: border-box;
	  width: 100%;
	  max-width: 720px;
	  margin: 0 auto;
	  padding: 24px;
	}

	.settings-pane-topbar {
	  display: flex;
	  align-items: center;
	  gap: 10px;
	  margin: 0 0 14px;
	}

	.settings-pane-title {
	  margin: 0;
	  font-size: 22px;
	  font-weight: 700;
	  line-height: 1.25;
	}

	.settings-hamburger {
	  display: none;
	  align-items: center;
	  justify-content: center;
	  width: 34px;
	  height: 34px;
	  padding: 0;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-control);
	  background: var(--fill);
	  color: var(--fg);
	  cursor: pointer;
	}

	.settings-route-content {
	  min-width: 0;
	}

	.settings-home {
	  display: grid;
	  gap: 16px;
	}

	.settings-status-strip {
	  display: grid;
	  gap: 8px;
	  padding: 14px 16px;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-card);
	  background: var(--bg-input);
	}

	.settings-status-line,
	.settings-status-button {
	  display: grid;
	  grid-template-columns: 116px minmax(0, 1fr);
	  gap: 12px;
	  align-items: center;
	  min-height: 30px;
	  font-size: 13px;
	}

	.settings-status-button {
	  width: 100%;
	  border: 1px solid transparent;
	  border-radius: var(--radius-control);
	  background: transparent;
	  color: var(--fg);
	  text-align: left;
	  cursor: pointer;
	}

	.settings-status-label {
	  color: var(--fg-subtle);
	}

	.settings-status-value {
	  min-width: 0;
	  overflow-wrap: anywhere;
	}

	.settings-card-grid {
	  display: grid;
	  grid-template-columns: repeat(2, minmax(0, 1fr));
	  gap: 12px;
	}

	.settings-card {
	  display: grid;
	  gap: 12px;
	  align-content: start;
	  min-width: 0;
	  padding: 15px;
	  border: 1px solid var(--border);
	  border-radius: var(--radius-card);
	  background: var(--fill);
	}

	.settings-card-title {
	  margin: 0;
	  font-size: 12px;
	  font-weight: 600;
	  color: var(--fg-subtle);
	}

	.settings-card-glance {
	  min-width: 0;
	  font-size: 13px;
	  overflow-wrap: anywhere;
	}

	.settings-card-action {
	  align-self: end;
	}

	@media (max-width: 719px) {
	  .settings-shell {
	    grid-template-columns: minmax(0, 1fr);
	  }

	  .settings-pane-frame {
	    padding: 12px;
	  }

	  .settings-hamburger {
	    display: inline-flex;
	    flex: 0 0 auto;
	  }

	  .settings-rail {
	    position: absolute;
	    inset: 0 auto 0 0;
	    z-index: 3;
	    width: min(280px, calc(100% - 48px));
	    visibility: hidden;
	    transform: translateX(-100%);
	    transition: transform var(--dur-fast) var(--ease-standard), visibility var(--dur-fast) var(--ease-standard);
	  }

	  .settings-shell[data-pane-open="true"] .settings-rail {
	    visibility: visible;
	    transform: translateX(0);
	  }

	  .settings-scrim {
	    display: block;
	    position: absolute;
	    inset: 0;
	    z-index: 2;
	    background: rgba(0,0,0,0.32);
	    opacity: 0;
	    visibility: hidden;
	    transition: opacity var(--dur-fast) var(--ease-standard), visibility var(--dur-fast) var(--ease-standard);
	  }

	  .settings-shell[data-pane-open="true"] .settings-scrim {
	    opacity: 1;
	    visibility: visible;
	  }

	  .settings-card-grid {
	    grid-template-columns: minmax(0, 1fr);
	  }

	  .settings-status-line,
	  .settings-status-button {
	    grid-template-columns: 92px minmax(0, 1fr);
	  }
	}

	@media (prefers-reduced-motion: no-preference) {
	  .fluent-control,
	  .fluent-accent,
	  .settings-nav-item,
	  .settings-status-button,
	  .settings-hamburger {
	    transition: box-shadow var(--dur-fast) var(--ease-standard);
	  }

  @keyframes update-indeterminate {
    0% { left: -35%; }
    100% { left: 100%; }
  }
}
`;

function isTextEntryTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement;
}

// True while the owner is mid-interaction with a control a full rerender would
// disrupt: an open native <select> popup (the picker / frequency
// dropdowns), in-progress text entry, or hotkey capture. Distinct from
// isTextEntryTarget — it reads document.activeElement and includes <select>.
function isInteractiveControlActive(): boolean {
  const active = document.activeElement;
  return (
    active instanceof HTMLSelectElement ||
    (active instanceof HTMLInputElement && active.type !== "checkbox" && active.type !== "radio") ||
    active instanceof HTMLTextAreaElement ||
    hotkeyCapturing
  );
}

function settingsShell(): HTMLElement | null {
  return document.querySelector<HTMLElement>(".settings-shell");
}

function openNavOverlay(): void {
  const shell = settingsShell();
  if (!shell) {
    return;
  }
  shell.dataset.paneOpen = "true";
  const hamburger = document.querySelector<HTMLElement>(".settings-hamburger");
  if (hamburger) {
    hamburger.setAttribute("aria-expanded", "true");
    hamburger.setAttribute("aria-label", t("windows.ui.closenavigation"));
  }
  document.querySelector<HTMLElement>(".settings-rail .settings-nav-item")?.focus();
}

function closeNavOverlay(returnFocusToHamburger = false): void {
  const shell = settingsShell();
  if (!shell) {
    return;
  }
  const wasOpen = navOverlayOpen(shell);
  shell.dataset.paneOpen = "false";
  const hamburger = document.querySelector<HTMLElement>(".settings-hamburger");
  if (hamburger) {
    hamburger.setAttribute("aria-expanded", "false");
    hamburger.setAttribute("aria-label", t("windows.ui.opennavigation"));
  }
  if (returnFocusToHamburger && wasOpen) {
    hamburger?.focus();
  }
}

function navOverlayOpen(shell: HTMLElement): boolean {
  return shell.dataset.paneOpen === "true";
}

function text(tag: keyof HTMLElementTagNameMap, value: string): HTMLElement {
  const node = document.createElement(tag);
  node.textContent = value;
  return node;
}

function automation(node: HTMLElement, id: string): HTMLElement {
  node.dataset.automationId = id;
  return node;
}

function selectable(node: HTMLElement): HTMLElement {
  node.classList.add("selectable");
  return node;
}

function section(title: string): HTMLElement {
  const node = document.createElement("section");
  node.style.padding = "18px 20px";
  node.style.borderBottom = "1px solid var(--border)";

  const heading = text("h2", title);
  heading.style.margin = "0 0 12px";
  heading.style.fontSize = "13px";
  heading.style.fontWeight = "700";
  heading.style.textTransform = "uppercase";
  heading.style.letterSpacing = "0";
  heading.style.color = "var(--fg-subtle)";
  node.append(heading);

  return node;
}

function valueRow(labelText: string, value: HTMLElement): HTMLDivElement {
  const row = document.createElement("div");
  row.style.display = "grid";
  row.style.gridTemplateColumns = "132px minmax(0, 1fr)";
  row.style.gap = "12px";
  row.style.alignItems = "start";
  row.style.minHeight = "30px";
  row.style.padding = "7px 0";

  const labelNode = text("div", labelText);
  labelNode.style.color = "var(--fg-subtle)";
  labelNode.style.fontSize = "13px";
  value.style.fontSize = "13px";
  value.style.overflowWrap = "anywhere";
  row.append(labelNode, value);
  return row;
}

// ── Settings experience vocabulary ───────────────────────────────────────────
// The within-section building blocks the Updates pass established (orienting
// caption → subhead → helper caption → trust footnote) so every Settings
// section reads as a status/control surface, not a flat label:value table.

// An orienting line under a section heading or a subhead (the macOS GroupBox caption).
function helpCaption(value: string): HTMLElement {
  const d = text("div", value);
  d.style.color = "var(--fg-subtle)";
  d.style.fontSize = "12px";
  d.style.lineHeight = "1.45";
  d.style.margin = "0 0 8px";
  return d;
}

// A group label within a section (e.g. "device priority", "your shortcut").
function subheadLabel(value: string): HTMLElement {
  const d = text("div", value);
  d.style.fontSize = "12px";
  d.style.fontWeight = "600";
  d.style.color = "var(--fg-subtle)";
  d.style.margin = "14px 0 4px";
  return d;
}

// A trailing caption beneath a control (the macOS "changes take effect…" line).
function microCaption(value: string): HTMLElement {
  const d = text("div", value);
  d.style.color = "var(--fg-subtle)";
  d.style.fontSize = "12px";
  d.style.lineHeight = "1.4";
  d.style.margin = "4px 0 0";
  return d;
}

function pill(label: string, severity: Severity): HTMLElement {
  const colors: Record<Severity, { text: string; bg: string; border: string }> = {
    ok: {
      text: "var(--accent)",
      bg: "var(--accent-subtle)",
      border: "1px solid var(--accent)",
    },
    neutral: {
      text: "var(--fg-subtle)",
      bg: "var(--fill)",
      border: "1px solid var(--border)",
    },
    attention: {
      text: "var(--danger)",
      bg: "var(--fill)",
      border: "1px solid var(--danger)",
    },
  };

  const chip = document.createElement("span");
  const color = colors[severity];
  chip.style.display = "inline-flex";
  chip.style.alignItems = "center";
  chip.style.width = "fit-content";
  chip.style.maxWidth = "100%";
  chip.style.boxSizing = "border-box";
  chip.style.fontSize = "12px";
  chip.style.fontWeight = "500";
  chip.style.lineHeight = "1.35";
  chip.style.padding = "2px 9px";
  chip.style.borderRadius = "999px";
  chip.style.color = color.text;
  chip.style.background = color.bg;
  chip.style.border = color.border;

  const value = text("span", label);
  value.style.minWidth = "0";
  value.style.overflowWrap = "anywhere";
  chip.append(value);
  return chip;
}

function phaseLabel(phase: AppPhase): string {
  switch (phase) {
    case "idle":
      return t("windows.ui.idle");
    case "starting":
      return t("windows.tray.starting");
    case "observing":
      return t("windows.desktop.shared.app.settingsdiagnosticson");
    case "paused":
      return t("windows.ui.pausedxa7a9dc5b");
    case "error":
      return t("windows.desktop.shared.copy.settingstabattentiona11y");
  }
}

function isAccessDenied(report: SourceReport | undefined): boolean {
  return report?.status === "faulted" && (report.reason === "access_denied" || report.reason === "AccessDenied");
}

const NO_MICROPHONE_LINE =
  t("windows.ui.nomicrophoneisavailableconnectorenableoneinwindo");
const NO_OUTPUT_LINE =
  t("windows.ui.nosoundoutputisavailableconnectorenableoneinwind");
const ACCESS_DENIED_LINE = t("windows.ui.accesswasdeniedcheckwindowspermissionsfor");
const NO_MICROPHONE_TILE = t("windows.ui.nomicrophonefound");
const NO_OUTPUT_TILE = t("windows.ui.nosoundoutputfound");
const ACCESS_DENIED_TILE = t("windows.ui.accesswasdenied");
const BROWSER_TILE_SETUP = t("windows.ui.addthesolstoneextension");
const OBSIDIAN_TILE_SETUP = t("windows.ui.chooseafolder");

// A source's raw fault detail is a diagnostic for logs and the health dump;
// the pill carries only the status word.
function sourcePill(
  status: "off" | "paused" | "on" | "unavailable" | "error" | null,
  needsPermission = false,
): HTMLElement {
  if (status === null) {
    const el = document.createElement("span");
    el.classList.add("source-status");
    return el;
  }
  const severity: Severity =
    status === "on" ? "ok" : status === "error" || status === "unavailable" ? "attention" : "neutral";
  const word = status === "unavailable" && needsPermission ? t("windows.ui.needspermission") : stateWord(status);
  const el = pill(stateWord(word), severity);
  el.style.flexShrink = "0";
  el.style.whiteSpace = "nowrap";
  el.style.maxWidth = "100%";
  el.dataset.status = status;
  return el;
}

function sourceSaveFailureMessage(failure: FailedSourceWrite): string {
  return failure.attempted
    ? t("windows.ui.couldnotsaveturningthisonitstaysofftryagain")
    : t("windows.ui.couldnotsaveturningthisoffitisoffnowbutmayturnon");
}

function sourceByKind(dump: HealthDump, kind: SourceKind): SourceReport | undefined {
  return dump.sources.find((source) => source.kind === kind);
}

function failedPairingLabel(detail: string | null | undefined): string {
  switch (detail) {
    case "pair_link":
      return t("windows.ui.thatpairinglinkisntonethe");
    case "relay_pair_window_closed":
      return t("windows.ui.thepairingwindowclosedgetanew");
    case "io":
      return t("windows.ui.thisdeviceisntonanetwork");
    case "relay_unpaid":
      return t("windows.ui.couldntreachyourjournaloveryour");
    case "journal_refused":
      return t("windows.ui.thispccantconnecttoyour");
    case "storage_unavailable":
      return t("windows.ui.text", { "name": String(MIGRATION_COPY["migration.storage_unavailable.title"]), "value2": String(MIGRATION_COPY["migration.storage_unavailable.body"]) });
    default:
      return t("windows.ui.pairingdidntgothroughgeta");
  }
}

function markTextForm(mark: MarkRenderSpec | null | undefined): string {
  if (!mark) {
    return t("windows.ui.markunavailable");
  }
  return `${mark.words[0].toLowerCase()}\u00B7${mark.words[1].toLowerCase()}`;
}

// journal_label stays on the dump for compatibility and is never rendered.
function pairingPhaseLabel(pairing: PairingState): string {
  switch (pairing.phase) {
    case "not_paired":
      return t("windows.desktop.shared.app.notpaired");
    case "pairing":
      return t("windows.ui.textcontent2");
    case "awaiting_confirmation":
      return pairing.finish_connecting
        ? t("windows.ui.finishconnectingtoyourjournal")
        : t("windows.ui.waitingforyoutoconfirmyourjournalsmark");
    case "paired":
      return t("windows.ui.pairedwith", { "value": String(markTextForm(pairing.mark)) });
    case "failed":
      return failedPairingLabel(pairing.detail);
  }
}

function presentedPairing(pairing: PairingState): PairingState {
  if (pairingBusy && pairing.phase === "awaiting_confirmation" && !pairing.finish_connecting) {
    return { ...pairing, phase: "pairing" };
  }
  return pairing;
}

function quarantinedCount(upload: UploadStatus): number {
  return upload.quarantined_segments ?? 0;
}

function needsAttentionLabel(count: number): string {
  return count === 1 ? t("windows.ui.value1needsattention") : t("windows.ui.arg0needattention", { "arg0": String(count) });
}

function uploadLabel(upload: UploadStatus): string {
  const parts = [
    t("windows.ui.delivered", { "value": String(upload.uploaded_segments) }),
    t("windows.ui.pending", { "value": String(upload.pending_segments) }),
  ];
  if (upload.failed_segments > 0) {
    parts.push(t("windows.ui.retrying", { "value": String(upload.failed_segments) }));
  }
  const quarantined = quarantinedCount(upload);
  if (quarantined > 0) {
    parts.push(needsAttentionLabel(quarantined));
  }
  return parts.join(" · ");
}

function formatStorageBytes(bytes: number): string {
  const units = ["bytes", "kb", "mb", "gb", "tb"];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const rendered = unit === 0 ? String(Math.floor(value)) : value.toFixed(1);
  switch (unit) {
    case 0: return t("windows.ui.arg0bytes", { "arg0": rendered });
    case 1: return t("windows.ui.arg0kbx75b11ba1", { "arg0": rendered });
    case 2: return t("windows.ui.arg0mbxff882f00", { "arg0": rendered });
    case 3: return t("windows.ui.arg0gb", { "arg0": rendered });
    case 4: return t("windows.ui.arg0tb", { "arg0": rendered });
    default: return rendered;
  }
}

function storageRow(storage: StorageInfo | null): HTMLElement {
  const value = document.createElement("div");
  value.style.display = "grid";
  value.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  value.style.gap = "8px";
  value.style.alignItems = "start";

  const pathWrap = document.createElement("div");
  pathWrap.style.minWidth = "0";
  pathWrap.append(
    selectable(
      automation(
        text("div", storage ? storage.root : t("windows.ui.notavailable")),
        ids["settings.status.segmentDir"],
      ),
    ),
  );
  if (storage && storage.bytes !== null) {
    pathWrap.append(microCaption(t("windows.ui.storedlocally", { "value": String(formatStorageBytes(storage.bytes)) })));
  }

  value.append(
    pathWrap,
    actionButton(t("windows.ui.openfolder"), undefined, true, () => void invoke("open_storage_folder")),
  );
  return valueRow(t("windows.ui.storedonthispc"), value);
}

function syncRow(sync: SyncSnapshot): HTMLElement {
  const pairing = presentedPairing(sync.pairing);
  let label: string;
  switch (pairing.phase) {
    case "not_paired":
      label = t("windows.ui.label");
      break;
    case "pairing":
    case "awaiting_confirmation":
    case "failed":
      label = pairingPhaseLabel(pairing);
      break;
    case "paired":
      label = uploadLabel(sync.upload);
      break;
  }

  return valueRow(
    t("windows.ui.journalsync"),
    selectable(automation(text("div", label), ids["settings.status.upload.state"])),
  );
}

function renderJournalOpenSection(dump: HealthDump): HTMLElement {
  const pane = section(t("windows.desktop.shared.copy.setupjournalcardheading"));
  const pairing = presentedPairing(dump.sync.pairing);

  if (pairing.phase === "awaiting_confirmation") {
    pane.append(
      automation(
        text("div", pairing.finish_connecting ? pairingPhaseLabel(pairing) : t("windows.ui.confirmyourjournalsmarktoopen")),
        ids["settings.journal.unavailable"],
      ),
    );
    return pane;
  }

  if (pairing.phase !== "paired") {
    journalOpenError = false;
    pane.append(
      automation(
        text("div", t("windows.ui.pairthispctoyourjournalto")),
        ids["settings.journal.unavailable"],
      ),
    );
    return pane;
  }

  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.flexWrap = "wrap";
  row.style.alignItems = "center";
  row.style.gap = "10px";
  row.style.padding = "7px 0";

  const error = text("div", t("windows.ui.couldntopenyourjournaltryagain"));
  error.style.color = "var(--danger)";
  error.style.fontSize = "12px";
  error.hidden = !journalOpenError;

  const button = actionButton(t("windows.desktop.shared.copy.settingsjournalopen"), ids["settings.journal.open"], true, async () => {
    try {
      await invoke("open_journal");
      journalOpenError = false;
      error.hidden = true;
    } catch {
      journalOpenError = true;
      error.hidden = false;
    }
  });

  row.append(button, error);
  pane.append(row);
  return pane;
}

function migrationCard(): HTMLDivElement {
  const card = document.createElement("div");
  card.classList.add("fluent-card");
  card.style.display = "grid";
  card.style.gap = "10px";
  card.style.marginTop = "12px";
  card.style.padding = "14px";
  card.style.border = "1px solid var(--border)";
  card.style.borderRadius = "var(--radius-control)";
  card.style.background = "var(--fill)";
  return card;
}

function migrationHeading(card: HTMLElement, key: MigrationCopyKey, values?: Record<string, string>): void {
  const heading = migrationText("h3", key, values);
  heading.style.margin = "0";
  heading.style.fontSize = "14px";
  heading.style.fontWeight = "650";
  card.append(heading);
}

function migrationBody(card: HTMLElement, key: MigrationCopyKey, values?: Record<string, string>): void {
  const body = migrationText("p", key, values);
  body.style.margin = "0";
  body.style.fontSize = "12px";
  body.style.color = "var(--fg-subtle)";
  body.style.lineHeight = "1.45";
  card.append(body);
}

function migrationActions(card: HTMLElement, ...buttons: HTMLButtonElement[]): void {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.flexWrap = "wrap";
  row.style.justifyContent = "flex-end";
  row.style.gap = "8px";
  row.append(...buttons);
  card.append(row);
}

function renderMigrationIssue(issue: MigrationUiIssue): HTMLElement {
  const card = migrationCard();
  const retry = (key: MigrationCopyKey, action: () => void) => migrationAction(key, action);
  switch (issue) {
    case "offline":
      migrationHeading(card, "migration.offline.title");
      migrationBody(card, "migration.offline.body");
      migrationActions(card, retry("migration.offline.action", () => void refreshMigrationState(true)));
      break;
    case "unsupported":
      migrationHeading(card, "migration.unsupported.title");
      migrationBody(card, "migration.unsupported.body");
      migrationActions(card, retry("migration.unsupported.action", () => void refreshMigrationState(true)));
      break;
    case "storage_unavailable":
      migrationHeading(card, "migration.storage_unavailable.title");
      migrationBody(card, "migration.storage_unavailable.body");
      migrationActions(
        card,
        retry("migration.storage_unavailable.action", () => {
          migrationShowDetails = !migrationShowDetails;
          requestRerender();
        }),
      );
      if (migrationShowDetails) {
        const details = text("code", issue);
        details.dataset.migrationIssue = issue;
        card.append(details);
      }
      break;
    case "decision_refused":
      migrationHeading(card, "migration.decision_refused.title");
      migrationBody(card, "migration.decision_refused.body");
      migrationActions(
        card,
        retry("migration.decision_refused.action", () => {
          migrationShowDetails = !migrationShowDetails;
          requestRerender();
        }),
      );
      if (migrationShowDetails) {
        const details = text("code", issue);
        details.dataset.migrationIssue = issue;
        card.append(details);
      }
      break;
    case "target_missing":
      migrationHeading(card, "migration.target_missing.title");
      migrationBody(card, "migration.target_missing.body");
      migrationActions(
        card,
        retry("migration.target_missing.action", () => {
          latestMigration = {
            migration: latestMigration?.migration ?? null,
            issue: null,
          };
          migrationFlow = "picker";
          void loadMigrationDevices();
          requestRerender();
        }),
      );
      break;
    case "key_refused":
      migrationHeading(card, "migration.key_refused.title");
      migrationBody(card, "migration.key_refused.body");
      migrationActions(
        card,
        retry("migration.key_refused.action", () => {
          document.querySelector<HTMLInputElement>(`[data-automation-id="${ids["settings.pairing.input"]}"]`)?.focus();
        }),
      );
      break;
  }
  return card;
}

function migrationPendingRow(card: HTMLElement): void {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.justifyContent = "space-between";
  row.style.gap = "12px";
  const labelNode = migrationText("span", "migration.pending.row");
  const valueNode = migrationText("span", "migration.pending.value");
  row.append(labelNode, valueNode);
  card.append(row);
}

function renderMigrationFlow(): HTMLElement {
  const card = migrationCard();
  if (migrationFlow === "picker") {
    migrationHeading(card, "migration.picker.title");
    if (migrationDevicesBusy) {
      migrationHeading(card, "migration.preparing.title");
    } else if (migrationListUnavailable) {
      migrationHeading(card, "migration.list_unavailable.title");
      migrationBody(card, "migration.list_unavailable.body");
      migrationActions(
        card,
        migrationAction("migration.list_unavailable.action", () => void loadMigrationDevices()),
      );
    } else {
      const choices = latestMigrationDevices ?? [];
      if (choices.length === 0) {
        const empty = migrationText("p", "migration.picker.empty");
        empty.style.margin = "0";
        card.append(empty);
      }
      const list = document.createElement("div");
      list.setAttribute("role", "list");
      list.style.display = "grid";
      list.style.gap = "6px";
      for (const device of choices) {
        const button = document.createElement("button");
        button.type = "button";
        button.setAttribute("role", "listitem");
        button.textContent = device.display_label;
        button.dataset.migrationAction = "select_device";
        button.setAttribute("aria-pressed", String(selectedMigrationDevice?.cid === device.cid));
        button.classList.add("fluent-control");
        button.style.textAlign = "left";
        button.onclick = () => {
          selectedMigrationDevice = device;
          migrationFlow = "confirm";
          requestRerender();
        };
        list.append(button);
      }
      card.append(list);
    }
    migrationActions(
      card,
      migrationAction("migration.picker.cancel", () => {
        migrationFlow = "replace_offer";
        selectedMigrationDevice = null;
        requestRerender();
      }),
    );
    return card;
  }

  if (migrationFlow === "confirm") {
    migrationHeading(
      card,
      selectedMigrationDevice
        ? "migration.replace_confirm.title"
        : "migration.replace_confirm.title_fallback",
      selectedMigrationDevice
        ? { selected_device_label: selectedMigrationDevice.display_label }
        : undefined,
    );
    migrationBody(card, "migration.replace_confirm.body");
    migrationActions(
      card,
      migrationAction("migration.replace_confirm.cancel", () => {
        migrationFlow = "picker";
        requestRerender();
      }),
      migrationAction("migration.replace_confirm.replace", () => {
        const cid = selectedMigrationDevice?.cid;
        if (cid) void submitMigrationDecision("replace_device", cid);
      }),
    );
    return card;
  }

  migrationHeading(card, "migration.replace_offer.title");
  migrationBody(card, "migration.replace_offer.body");
  migrationActions(
    card,
    migrationAction("migration.replace_offer.defer", () => void dismissMigrationOffer()),
    migrationAction("migration.replace_offer.keep", () => void submitMigrationDecision("new_device")),
    migrationAction("migration.replace_offer.pick", () => {
      migrationFlow = "picker";
      void loadMigrationDevices();
      requestRerender();
    }),
  );
  return card;
}

function renderMigrationPanel(): HTMLElement | null {
  const snapshot = latestMigration;
  const view = snapshot?.migration;
  if (snapshot?.issue) return renderMigrationIssue(snapshot.issue);
  if (migrationBusy && view) {
    const card = migrationCard();
    migrationHeading(card, "migration.deciding.title");
    return card;
  }
  if (!view) return null;

  if (view.phase === "decision_unknown" || view.decision_result === "unknown") {
    const card = migrationCard();
    migrationHeading(card, "migration.decision_unknown.title");
    migrationBody(card, "migration.decision_unknown.body");
    migrationPendingRow(card);
    migrationActions(
      card,
      migrationAction("migration.decision_unknown.action", () => void refreshMigrationState(true)),
    );
    return card;
  }

  if (migrationFlow === "replace_offer" || migrationFlow === "picker" || migrationFlow === "confirm") {
    return renderMigrationFlow();
  }
  if (!view.offer_available || migrationPromptHidden) return null;
  return renderMigrationFlow();
}

function renderPairingSection(dump: HealthDump, options: { setupStep?: boolean } = {}): HTMLElement {
  const pairing = dump.sync.pairing;
  const presented = presentedPairing(pairing);
  // Once setup's connect step has paired, opening the journal is the only action
  // left there; pasting another link stays available from the journal page.
  const connectedInSetup = options.setupStep === true && pairing.phase === "paired";
  const pane = section(t("windows.ui.pairing"));
  if (!connectedInSetup) {
    pairingStatusEl = text("div", pairingStatusLabel(dump));
    pane.append(
      valueRow(
        t("windows.ui.status"),
        selectable(automation(pairingStatusEl, ids["settings.pairing.state"])),
      ),
    );
  }

  if (presented.phase === "awaiting_confirmation") {
    if (dump.setup?.sequence_enabled === true && pairing.finish_connecting) {
      const card = document.createElement("div");
      card.style.marginTop = "12px";
      const finishBtn = actionButton(
        t("windows.ui.finishconnecting"),
        ids["settings.pairing.finishConnecting"],
        !finishInFlight,
        async () => {
          if (finishInFlight) return;
          finishInFlight = true;
          try {
            await invoke("finish_connecting", { binding: pairing.binding });
            setupActionError.finish = "";
          } catch (error) {
            setupActionError.finish = errorText(error);
          } finally {
            finishInFlight = false;
            retryHealth();
          }
        },
        finishInFlight,
        "accent",
      );
      card.append(finishBtn);
      appendSetupError(card, "finish");
      pane.append(card);
      return pane;
    }

    const card = document.createElement("div");
    card.style.marginTop = "12px";

    if (displayableMark(pairing.mark)) {
      const markRow = renderConfirmCard(pairing.mark);

      const emphasis = text("div", t("windows.desktop.shared.copy.journalmarkconfirmquestion"));
      emphasis.style.fontWeight = "600";
      emphasis.style.fontSize = "14px";
      emphasis.style.marginBottom = "4px";
      emphasis.style.textAlign = "center";

      const subtle = text(
        "div",
        t("windows.ui.yourjournalshowsthissamemarkbeside"),
      );
      subtle.style.fontSize = "12px";
      subtle.style.color = "var(--fg-subtle)";
      subtle.style.marginBottom = "14px";
      subtle.style.textAlign = "center";

      const btnRow = document.createElement("div");
      btnRow.style.display = "flex";
      btnRow.style.justifyContent = "flex-end";
      btnRow.style.gap = "8px";
      btnRow.style.marginTop = "8px";

      const rejectBtn = actionButton(
        t("windows.desktop.shared.copy.journalmarkmismatchbutton"),
        ids["settings.pairing.confirmReject"],
        true,
        async () => {
          if (pairingBusy) return;
          pairingBusy = true;
          try {
            await invoke("answer_pairing", { binding: pairing.binding ?? "", action: "reject" });
          } catch {
            // Handled via pairing status updates
          } finally {
            pairingBusy = false;
          }
        },
        pairingBusy,
        "standard",
      );

      const confirmBtn = actionButton(
        t("windows.desktop.shared.copy.journalmarkconfirmbutton"),
        ids["settings.pairing.confirmYes"],
        true,
        async () => {
          if (pairingBusy) return;
          pairingBusy = true;
          try {
            await invoke("answer_pairing", { binding: pairing.binding ?? "", action: "confirm" });
          } catch {
            // Handled via pairing status updates
          } finally {
            pairingBusy = false;
          }
        },
        pairingBusy,
        "accent",
      );

      btnRow.append(rejectBtn, confirmBtn);
      card.append(markRow, emphasis, subtle, btnRow);
    } else {
      const markRow = renderConfirmCard(null);

      const emphasis = text("div", t("windows.desktop.shared.copy.journalmarkunverifiedtitle"));
      emphasis.style.fontWeight = "600";
      emphasis.style.fontSize = "14px";
      emphasis.style.marginBottom = "4px";
      emphasis.style.textAlign = "center";

      const subtle = text(
        "div",
        t("windows.ui.thispccouldntworkoutyour"),
      );
      subtle.style.fontSize = "12px";
      subtle.style.color = "var(--fg-subtle)";
      subtle.style.marginBottom = "14px";
      subtle.style.textAlign = "center";

      const btnRow = document.createElement("div");
      btnRow.style.display = "flex";
      btnRow.style.justifyContent = "flex-end";
      btnRow.style.gap = "8px";
      btnRow.style.marginTop = "8px";

      const cancelBtn = actionButton(
        t("windows.desktop.shared.copy.journalmarkunverifiedcancelbutton"),
        ids["settings.pairing.confirmCancel"],
        true,
        async () => {
          if (pairingBusy) return;
          pairingBusy = true;
          try {
            await invoke("answer_pairing", { binding: pairing.binding ?? "", action: "cancel" });
          } catch {
            // Handled via pairing status updates
          } finally {
            pairingBusy = false;
          }
        },
        pairingBusy,
        "standard",
      );

      const continueBtn = actionButton(
        t("windows.desktop.shared.copy.journalmarkunverifiedcontinuebutton"),
        ids["settings.pairing.confirmContinue"],
        true,
        async () => {
          if (pairingBusy) return;
          pairingBusy = true;
          try {
            await invoke("answer_pairing", { binding: pairing.binding ?? "", action: "confirm" });
          } catch {
            // Handled via pairing status updates
          } finally {
            pairingBusy = false;
          }
        },
        pairingBusy,
        "accent",
      );

      btnRow.append(cancelBtn, continueBtn);
      card.append(markRow, emphasis, subtle, btnRow);
    }

    pane.append(card);
    return pane;
  }

  const paired = pairing.phase === "paired";
  const found = !paired && foundJournal && !nativeContinuationPending(dump) ? foundJournal : null;
  if (found) {
    pane.append(renderFoundJournal(found));
  } else {
    pane.append(
      valueRow(
        t("windows.ui.mark"),
        automation(
          renderMarkCard(paired ? pairing.mark : null, paired ? "unavailable" : "generic"),
          ids["settings.pairing.mark"],
        ),
      ),
    );
  }

  if (pairing.phase === "not_paired") {
    if (pairing.detail === SAME_COMPUTER_UNVERIFIED) {
      const msg = automation(
        text("div", t("windows.ui.couldntverifyjournalsonothing")),
        ids["settings.pairing.foundFailed"],
      );
      msg.style.fontSize = "12px";
      msg.style.color = "var(--fg-subtle)";
      msg.style.margin = "8px 0";
      pane.append(msg);
    } else if (pairing.detail === "mark_rejected") {
      const msg = text(
        "div",
        t("windows.ui.yousaidthismarkdoesntmatch"),
      );
      msg.style.fontSize = "12px";
      msg.style.color = "var(--fg-subtle)";
      msg.style.margin = "8px 0";
      pane.append(msg);
    } else if (pairing.detail === "pairing_cancelled") {
      const msg = text(
        "div",
        t("windows.ui.pairingcancellednothingthispchaskept"),
      );
      msg.style.fontSize = "12px";
      msg.style.color = "var(--fg-subtle)";
      msg.style.margin = "8px 0";
      pane.append(msg);
    }
  }

  const migrationPanel = renderMigrationPanel();
  if (migrationPanel) pane.append(migrationPanel);

  if (dump.setup?.sequence_enabled === true && !pairFieldDirty && !pairingDraft) {
    void Promise.resolve(invoke<string>("pairing_prefill"))
      .then((prefill) => {
        if (pairFieldDirty || pairingDraft) return;
        const link = typeof prefill === "string" ? prefill : "";
        if (!link || pairFieldDirty || pairingDraft) return;
        pairingDraft = link;
        input.value = link;
      })
      .catch(() => {});
  }

  if (dump.setup?.sequence_enabled === true && pairing.setup_browser_ready) {
    const browserRow = document.createElement("div");
    browserRow.style.marginTop = "8px";
    const openBtn = actionButton(
      t("windows.desktop.shared.copy.setupopenyourjournal"),
      ids["settings.setup.browserJournal"],
      !browserInFlight,
      async () => {
        if (browserInFlight) return;
        browserInFlight = true;
        try {
          await invoke("open_setup_journal", { binding: pairing.binding });
          setupActionError.browser = "";
        } catch (error) {
          setupActionError.browser = errorText(error);
        } finally {
          browserInFlight = false;
          retryHealth();
        }
      },
      browserInFlight,
    );
    browserRow.append(openBtn);
    appendSetupError(browserRow, "browser");
    pane.append(browserRow);
  }

  const handoffId = dump.setup?.handoff_id;
  const handoffRevision = dump.setup?.handoff_revision;
  if (
    dump.setup?.sequence_enabled === true && handoffId && handoffRevision !== undefined && handoffRevision !== null &&
    !dump.sync.pairing.finish_connecting &&
    !dump.sync.pairing.setup_browser_ready &&
    (dump.sync.pairing.phase === "not_paired" || dump.sync.pairing.phase === "failed" ||
      (dump.sync.pairing.phase === "paired" && dump.setup?.handoff_new_connection === true &&
        !dump.setup.handoff_expired))
  ) {
    const expired = Boolean(dump.setup?.handoff_expired);
    const connectRow = document.createElement("div");
    connectRow.style.marginTop = "8px";
    const connectBtn = actionButton(
      t("windows.ui.continuethisconnection"),
      ids["settings.setup.nativeConnect"],
      !expired && !nativeConnectInFlight,
      async () => {
        if (nativeConnectInFlight || expired) return;
        nativeConnectInFlight = true;
        try {
          await invoke("connect_native_handoff", {
            handoffId,
            revision: handoffRevision,
          });
          setupActionError.connect = "";
        } catch (error) {
          setupActionError.connect = errorText(error);
        } finally {
          nativeConnectInFlight = false;
          retryHealth();
        }
      },
      nativeConnectInFlight,
    );
    if (expired) connectBtn.disabled = true;
    connectRow.append(connectBtn);
    appendSetupError(connectRow, "connect");
    pane.append(connectRow);
  }

  const dismissDeferredButton = () =>
    actionButton(
      t("windows.desktop.shared.updates.dismiss"),
      ids["settings.setup.deferredDismiss"],
      !deferredInFlight,
      async () => {
        if (deferredInFlight) return;
        deferredInFlight = true;
        requestRerender();
        try {
          await invoke("dismiss_deferred_link");
          setupActionError.deferred = "";
        } catch (error) {
          setupActionError.deferred = errorText(error);
        } finally {
          deferredInFlight = false;
          retryHealth();
        }
      },
      deferredInFlight,
      "standard",
    );

  if (dump.setup?.sequence_enabled === true && dump.setup.has_deferred_link && !connectedInSetup) {
    const deferredNote = document.createElement("p");
    deferredNote.classList.add("setup-deferred-note");
    deferredNote.textContent = t("windows.ui.textcontent");
    pane.append(deferredNote);
    const deferredRow = document.createElement("div");
    deferredRow.classList.add("setup-deferred-row");
    deferredRow.style.marginTop = "8px";
    deferredRow.style.display = "flex";
    deferredRow.style.gap = "8px";
    const useBtn = actionButton(
      t("windows.ui.usethislink"),
      ids["settings.setup.deferredUse"],
      !deferredInFlight,
      async () => {
        if (deferredInFlight) return;
        deferredInFlight = true;
        requestRerender();
        try {
          const link = await invoke<string>("use_deferred_link");
          if (typeof link === "string" && link) {
            pairingDraft = link;
            pairFieldDirty = true;
            input.value = link;
          }
          setupActionError.deferred = "";
        } catch (error) {
          setupActionError.deferred = errorText(error);
        } finally {
          deferredInFlight = false;
          retryHealth();
        }
      },
      deferredInFlight,
      "standard",
    );
    deferredRow.append(useBtn, dismissDeferredButton());
    pane.append(deferredRow);
    appendSetupError(pane, "deferred");
  } else if (dump.setup?.sequence_enabled === true && dump.setup.handoff_rejected) {
    const rejectedRow = document.createElement("div");
    rejectedRow.style.marginTop = "8px";
    rejectedRow.append(dismissDeferredButton());
    pane.append(rejectedRow);
    appendSetupError(pane, "deferred");
  }

  if (dump.setup?.sequence_enabled === true && (
    dump.setup?.mailbox_busy ||
    dump.setup?.handoff_rejected ||
    dump.setup?.native_handoff_rejected ||
    dump.setup?.handoff_deferred ||
    dump.setup?.handoff_persist_failed
  )) {
    const retryRow = document.createElement("div");
    retryRow.style.marginTop = "8px";
    const reason = document.createElement("p");
    const setup = dump.setup;
    reason.textContent = setup?.handoff_persist_failed
      ? SETUP_LINK_ERRORS.handoff_persist_failed
      : setup?.mailbox_busy
        ? SETUP_LINK_ERRORS.handoff_busy
        : setup?.native_handoff_rejected
          ? SETUP_LINK_ERRORS.handoff_rejected
          : setup?.handoff_deferred
            ? SETUP_LINK_ERRORS.handoff_deferred
            : t("windows.ui.anotherpairinglinkcouldnotbeadded");
    retryRow.append(reason);
    const retryBtn = actionButton(
      t("windows.desktop.shared.copy.tryagain"),
      ids["settings.setup.linkRetry"],
      !retryInFlight,
      async () => {
        if (retryInFlight) return;
        retryInFlight = true;
        try {
          await invoke("retry_setup_link");
          setupActionError.retry = "";
        } catch (error) {
          setupActionError.retry = errorText(error);
        } finally {
          retryInFlight = false;
          retryHealth();
        }
      },
      retryInFlight,
    );
    retryRow.append(retryBtn);
    appendSetupError(retryRow, "retry");
    pane.append(retryRow);
  }

  const freshLink = document.createElement("p");
  freshLink.textContent = pairing.phase === "paired"
    ? t("windows.ui.anewlinkfromyourjournalcanbepastedhere")
    : t("windows.ui.inyourjournalchoosepairadeviceandpastethelinkher");
  freshLink.dataset.automationId = ids["settings.setup.freshLink"];
  if (!connectedInSetup) pane.append(freshLink);

  const inputRow = document.createElement("div");
  inputRow.style.display = "grid";
  inputRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  inputRow.style.gap = "8px";
  inputRow.style.padding = "7px 0";

  const input = document.createElement("input");
  input.type = "text";
  input.placeholder = t("windows.ui.placeholder");
  input.value = pairingDraft;
  input.dataset.automationId = ids["settings.pairing.input"];
  input.setAttribute("aria-label", t("windows.ui.pairinglinkfromyourjournal"));
  input.classList.add("fluent-control");
  input.style.fontSize = "13px";
  input.style.padding = "7px 9px";
  input.style.border = "1px solid var(--border)";
  input.style.borderRadius = "var(--radius-control)";
  input.style.background = "var(--bg-input)";
  input.style.color = "var(--fg)";
  input.style.minWidth = "0";
  input.oninput = () => {
    pairFieldDirty = true;
    pairingDraft = input.value;
  };

  inputRow.append(input);

  {
    const button = document.createElement("button");
    const busy = pairingBusy || pairing.phase === "pairing";
    button.textContent = busy ? t("windows.ui.textcontent2") : t("windows.desktop.shared.setup.pair");
    button.disabled = busy;
    button.dataset.automationId = ids["settings.pairing.submit"];
    // Beside an offer the connect button is the screen's one primary.
    button.classList.add(found ? "fluent-control" : "fluent-accent");
    button.style.fontSize = "13px";
    button.style.padding = "7px 14px";
    button.style.border = found ? "1px solid var(--border)" : "1px solid var(--accent)";
    button.style.borderRadius = "var(--radius-control)";
    button.style.background = found ? "var(--fill)" : busy ? "var(--accent-busy)" : "var(--accent)";
    button.style.color = found ? "var(--fg)" : "var(--accent-fg)";
    button.style.cursor = busy ? "default" : "pointer";
    button.onclick = async () => {
      const link = pairingDraft.trim();
      if (!link || pairingBusy) {
        return;
      }
      pairingBusy = true;
      foundJournalConnected = false;
      foundJournalConnectedAt = null;
      button.disabled = true;
      button.textContent = t("windows.ui.textcontent2");
      try {
        // The result is reflected through the health dump's pairing phase; we
        // ignore the return and let the next render paint the outcome.
        await invoke("pair", { link });
        // The link is spent once pairing succeeds. Keep the field only when a
        // different link replaced it while pairing ran.
        if (pairingDraft.trim() === link) {
          pairingDraft = "";
          pairFieldDirty = false;
          input.value = "";
        }
      } catch {
        // Failure surfaces as pairing.phase = "failed" with a detail.
      } finally {
        pairingBusy = false;
        rerender();
      }
    };
    inputRow.append(button);
  }

  if (!connectedInSetup) pane.append(inputRow);
  return pane;
}

function renderConfirmCard(mark: MarkRenderSpec | null): HTMLElement {
  const card = automation(renderMarkCard(mark, "unavailable"), ids["settings.pairing.markCard"]);
  card.setAttribute("tabindex", "-1");
  card.style.display = "flex";
  card.style.width = "fit-content";
  card.style.margin = "0 auto 14px";
  return card;
}

function renderUnknownJournalsSection(sightings: UnknownJournalSighting[] | undefined): HTMLElement | null {
  if (!sightings || sightings.length === 0) {
    return null;
  }

  const pane = section(t("windows.ui.unknownjournals"));
  automation(pane, ids["settings.unknown-journal.list"]);

  for (const sighting of sightings) {
    const details = document.createElement("details");
    details.classList.add("unknown-journal-details");
    details.style.marginBottom = "8px";

    const summary = document.createElement("summary");
    summary.classList.add("unknown-journal-summary");
    automation(summary, ids["settings.unknown-journal.summary"]);
    summary.style.cursor = "pointer";
    summary.style.fontSize = "13px";
    summary.style.fontWeight = "600";
    summary.style.color = "var(--fg)";
    summary.style.padding = "4px 0";

    const viewDetails = text("span", t("windows.ui.viewdetails"));
    viewDetails.style.marginLeft = "8px";
    viewDetails.style.fontWeight = "400";
    viewDetails.style.color = "var(--fg-subtle)";
    summary.append(
      text(
        "span",
        sighting.address
          ? t("windows.tray.unknownjournalseenat", { "name": String(sighting.address) })
          : t("windows.tray.unknownjournalseenthroughtherelay"),
      ),
      viewDetails,
    );
    details.append(summary);

    const detailContent = document.createElement("div");
    detailContent.classList.add("unknown-journal-detail");
    automation(detailContent, ids["settings.unknown-journal.detail"]);
    detailContent.style.marginTop = "12px";
    detailContent.style.display = "flex";
    detailContent.style.flexDirection = "column";
    detailContent.style.gap = "12px";

    const heading = text("div", t("windows.ui.somethingotherthanyourjournalansweredcompare"));
    heading.classList.add("unknown-journal-heading");
    heading.style.fontSize = "13px";
    heading.style.fontWeight = "600";

    const body = text(
      "p",
      sighting.address
        ? t("windows.ui.somethingansweredpcatisnt", { "value": String(sighting.address) })
        : t("windows.ui.somethingansweredpcthroughrelayisn"),
    );
    body.classList.add("unknown-journal-body");
    body.style.margin = "-8px 0 0";
    body.style.fontSize = "12px";
    body.style.color = "var(--fg-subtle)";
    detailContent.append(heading, body);

    const comparisonRow = document.createElement("div");
    comparisonRow.style.display = "grid";
    comparisonRow.style.gridTemplateColumns = "repeat(auto-fit, minmax(min(250px, 100%), 1fr))";
    comparisonRow.style.gap = "12px";

    // Responding / what answered
    const respondingBlock = document.createElement("div");
    automation(respondingBlock, ids["settings.unknown-journal.what-answered"]);
    const respondingLabel = text("div", t("windows.ui.whatanswered"));
    respondingLabel.style.fontSize = "11px";
    respondingLabel.style.color = "var(--fg-subtle)";
    respondingLabel.style.marginBottom = "6px";

    const respondingCaption = text(
      "div",
      displayableMark(sighting.responding_mark) ? t("windows.ui.claimednotverified") : t("windows.ui.noidentitypresented"),
    );
    automation(respondingCaption, ids["settings.unknown-journal.responding-caption"]);
    respondingCaption.style.fontSize = "11px";
    respondingCaption.style.color = "var(--fg-subtle)";
    respondingCaption.style.marginTop = "6px";

    respondingBlock.append(
      respondingLabel,
      renderMarkCard(sighting.responding_mark, "not-presented"),
      respondingCaption,
    );

    // Expected / your journal
    const pairedBlock = document.createElement("div");
    automation(pairedBlock, ids["settings.unknown-journal.your-journal"]);
    const pairedLabel = text("div", t("windows.desktop.shared.copy.setupjournalcardheading"));
    pairedLabel.style.fontSize = "11px";
    pairedLabel.style.color = "var(--fg-subtle)";
    pairedLabel.style.marginBottom = "6px";
    pairedBlock.append(pairedLabel, renderMarkCard(sighting.expected_mark, "unavailable"));

    comparisonRow.append(respondingBlock, pairedBlock);
    detailContent.append(comparisonRow);
    details.append(detailContent);
    pane.append(details);
  }

  return pane;
}

async function applyExclusions(next: ExclusionRules, edit: ExclusionEdit): Promise<void> {
  if (exclusionsPending) return;
  exclusionsPending = edit;
  rerender();
  let ok = true;
  let persisted = true;
  try {
    const outcome = await invoke<SetExclusionsOutcome>("set_exclusions", { rules: next });
    persisted = outcome.persisted;
  } catch {
    ok = false;
  }
  // Settled rules always come from the refetch (earned), never the optimistic
  // copy. set_exclusions is synchronous backend-side, so a short delay is enough
  // to keep the pending affordance perceptible before the honest state lands.
  setTimeout(() => {
    void invoke<ExclusionRules>("get_exclusions")
      .then((rules) => {
        latestExclusions = rules;
        if (ok) exclusionsPersisted = persisted;
      })
      .catch(() => {})
      .finally(() => {
        exclusionsPending = null;
        rerender();
      });
  }, 150);
}

function exclusionActivityLabel(health: ExclusionHealth | null): string {
  if (!health || !health.rules_active) return t("windows.ui.noexclusionsactive");
  const dropped = health.frames_dropped;
  const redacted = health.frames_redacted;
  if (redacted > 0) return t("windows.exclusions.activityredacted", {count: dropped, display: number(dropped, {useGrouping:false}), more: number(redacted, {useGrouping:false})});
  return t("windows.exclusions.activity", { count: dropped, display: number(dropped, { useGrouping: false }) });
}

// A removable list of string values (excluded exes / title keywords).
function removableList(
  values: string[],
  listAutomationId: string,
  onRemove: (value: string) => void,
  emptyText = t("windows.desktop.shared.app.emptytext"),
  pendingValue: string | null = null,
): HTMLElement {
  const list = automation(document.createElement("div"), listAutomationId);
  list.style.display = "flex";
  list.style.flexWrap = "wrap";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (values.length === 0) {
    const empty = text("div", emptyText);
    empty.style.color = "var(--muted)";
    empty.style.fontSize = "13px";
    list.append(empty);
    return list;
  }
  for (const value of values) {
    const chip = document.createElement("span");
    chip.style.display = "inline-flex";
    chip.style.alignItems = "center";
    chip.style.gap = "6px";
    chip.style.fontSize = "13px";
    chip.style.padding = "3px 4px 3px 9px";
    chip.style.border = "1px solid var(--border)";
    chip.style.borderRadius = "var(--radius-control)";
    chip.style.background = "var(--fill)";
    chip.append(text("span", value));

    const remove = document.createElement("button");
    remove.textContent = "×";
    remove.setAttribute("aria-label", t("windows.ui.remove", { "value": String(value) }));
    remove.style.border = "none";
    remove.style.background = "transparent";
    remove.style.color = "var(--fg-subtle)";
    remove.style.cursor = "pointer";
    remove.style.fontSize = "15px";
    remove.style.lineHeight = "1";
    remove.style.padding = "0 2px";
    if (value === pendingValue) {
      remove.disabled = true;
      remove.setAttribute("aria-busy", "true");
    } else {
      remove.onclick = () => onRemove(value);
    }
    chip.append(remove);
    list.append(chip);
  }
  return list;
}

function renderExclusionsSection(rules: ExclusionRules, dump: HealthDump, desk = false): HTMLElement {
  const pane = desk ? document.createElement("div") : section(t("windows.ui.privacy"));
  if (!desk) {
    pane.append(
      helpCaption(t("windows.ui.choosewhatstaysoutofyourjournal")),
    );
  }
  if (!exclusionsPersisted) {
    const warn = helpCaption(
      t("windows.ui.theserulesareactivenowbutcouldn"),
    );
    warn.style.color = "var(--danger)";
    pane.append(warn);
  }

  // Private browsing — title-heuristic auto-exclude, on by default. Only the
  // browsers measured to write their private mode into the window title are
  // named (see observer-exclusion); every other browser is stated as not seen.
  if (desk) {
    pane.append(
      deskSwitchRow(
        t("windows.ui.privatewindows"),
        rules.exclude_private_browsing,
        t("windows.ui.edgeandfirefoxsettoenglish"),
        (on) => {
          void applyExclusions({ ...rules, exclude_private_browsing: on }, { kind: "toggle" });
        },
      ),
    );
  } else {
    pane.append(
      toggleRow(
        t("windows.ui.keepprivatewindowsoutofyourjournal"),
        ids["settings.exclusions.privateBrowsing"],
        rules.exclude_private_browsing,
        (on) => {
          void applyExclusions({ ...rules, exclude_private_browsing: on }, { kind: "toggle" });
        },
        exclusionsPending?.kind === "toggle",
      ),
    );
    pane.append(
      microCaption(
        t("windows.ui.thisreadsthewindowtitleandmatches"),
      ),
    );
  }

  // Excluded apps — pick from the live running-app list (robust process identity).
  pane.append(subheadLabel(t("windows.desktop.shared.settings.excludedapps")));
  pane.append(helpCaption(t("windows.ui.windowsfromtheseappsneverreachyour")));
  const appPickRow = document.createElement("div");
  appPickRow.style.display = "grid";
  appPickRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  appPickRow.style.gap = "8px";
  appPickRow.style.padding = "7px 0";

  const appSelect = document.createElement("select");
  appSelect.dataset.automationId = ids["settings.exclusions.appInput"];
  appSelect.setAttribute("aria-label", t("windows.ui.chooseanapptoexclude"));
  appSelect.classList.add("fluent-control");
  appSelect.style.fontSize = "13px";
  appSelect.style.padding = "7px 9px";
  appSelect.style.border = "1px solid var(--border)";
  appSelect.style.borderRadius = "var(--radius-control)";
  appSelect.style.minWidth = "0";
  const choices = runningApps.filter(
    (app) =>
      app.exe_name.toLowerCase() !== SELF_EXE &&
      !rules.excluded_exes.includes(app.exe_name),
  );
  const placeholder = document.createElement("option");
  placeholder.value = "";
  placeholder.textContent = choices.length > 0 ? t("windows.ui.choosearunningapp") : t("windows.ui.nootherappsrunning");
  appSelect.append(placeholder);
  for (const app of choices) {
    const opt = document.createElement("option");
    opt.value = app.exe_name;
    opt.textContent =
      app.exe_name === "applicationframehost.exe"
        ? STORE_APPS_LABEL
        : app.display_name || app.exe_name;
    opt.title = app.exe_name;
    appSelect.append(opt);
  }

  const appAdd = actionButton(
    t("windows.ui.exclude"),
    ids["settings.exclusions.appAdd"],
    choices.length > 0,
    () => {
      const exe = appSelect.value.trim().toLowerCase();
      if (!exe || rules.excluded_exes.includes(exe)) {
        return;
      }
      void applyExclusions(
        { ...rules, excluded_exes: [...rules.excluded_exes, exe] },
        { kind: "appAdd" },
      );
    },
    exclusionsPending?.kind === "appAdd",
  );
  appPickRow.append(appSelect, appAdd);
  pane.append(appPickRow);
  pane.append(
    removableList(
      rules.excluded_exes,
      ids["settings.exclusions.appsList"],
      (exe) => {
        void applyExclusions(
          {
            ...rules,
            excluded_exes: rules.excluded_exes.filter((e) => e !== exe),
          },
          { kind: "appRemove", value: exe },
        );
      },
      t("windows.ui.nothingexcludedyet"),
      exclusionsPending?.kind === "appRemove" ? exclusionsPending.value : null,
    ),
  );

  // Title keywords — case-insensitive substring of a window title.
  pane.append(subheadLabel(t("windows.ui.titlekeywords")));
  pane.append(helpCaption(t("windows.ui.windowswhosetitlecontainsawordyou")));
  const titleRow = document.createElement("div");
  titleRow.style.display = "grid";
  titleRow.style.gridTemplateColumns = "minmax(0, 1fr) auto";
  titleRow.style.gap = "8px";
  titleRow.style.padding = "7px 0";

  const titleInput = document.createElement("input");
  titleInput.type = "text";
  titleInput.placeholder = t("windows.ui.placeholder2");
  titleInput.value = titleDraft;
  titleInput.dataset.automationId = ids["settings.exclusions.titleInput"];
  titleInput.setAttribute("aria-label", t("windows.ui.titlekeywordtoexclude"));
  titleInput.classList.add("fluent-control");
  titleInput.style.fontSize = "13px";
  titleInput.style.padding = "7px 9px";
  titleInput.style.border = "1px solid var(--border)";
  titleInput.style.borderRadius = "var(--radius-control)";
  titleInput.style.background = "var(--bg-input)";
  titleInput.style.color = "var(--fg)";
  titleInput.style.minWidth = "0";
  titleInput.oninput = () => {
    titleDraft = titleInput.value;
  };
  const addTitle = (): void => {
    const keyword = titleDraft.trim().toLowerCase();
    if (!keyword || rules.title_patterns.includes(keyword)) {
      return;
    }
    titleDraft = "";
    void applyExclusions(
      { ...rules, title_patterns: [...rules.title_patterns, keyword] },
      { kind: "titleAdd" },
    );
  };
  titleInput.onkeydown = (event) => {
    if (event.key === "Enter") {
      addTitle();
    }
  };

  const titleAdd = actionButton(
    t("windows.desktop.shared.settings.add"),
    ids["settings.exclusions.titleAdd"],
    true,
    addTitle,
    exclusionsPending?.kind === "titleAdd",
  );
  titleRow.append(titleInput, titleAdd);
  pane.append(titleRow);
  pane.append(
    removableList(
      rules.title_patterns,
      ids["settings.exclusions.titlesList"],
      (keyword) => {
        void applyExclusions(
          {
            ...rules,
            title_patterns: rules.title_patterns.filter((k) => k !== keyword),
          },
          { kind: "titleRemove", value: keyword },
        );
      },
      t("windows.ui.nokeywordsyet"),
      exclusionsPending?.kind === "titleRemove" ? exclusionsPending.value : null,
    ),
  );

  pane.append(
    microCaption(
      t("windows.ui.awindowthatclosesormovescan"),
    ),
  );

  // Exclusion activity — the never-silent surface (kept out/dropped this session),
  // landed as a labeled trust footnote: the proof the exclusions are working.
  const activityFoot = document.createElement("div");
  activityFoot.style.marginTop = "16px";
  activityFoot.style.paddingTop = "12px";
  activityFoot.style.borderTop = "1px solid var(--border-subtle)";
  const activityCap = text("div", t("windows.ui.exclusionactivity"));
  activityCap.style.fontSize = "12px";
  activityCap.style.fontWeight = "600";
  activityCap.style.color = "var(--fg-subtle)";
  activityCap.style.margin = "0 0 3px";
  const activityVal = automation(
    text("div", exclusionActivityLabel(dump.exclusions)),
    ids["settings.exclusions.activity"],
  );
  activityVal.style.fontSize = "12px";
  activityVal.style.color = "var(--fg-subtle)";
  activityVal.style.lineHeight = "1.45";
  activityFoot.append(activityCap, activityVal);
  pane.append(activityFoot);

  return pane;
}

function resetRoot(rootId: string): void {
  root.replaceChildren();
  pauseStatusEls = [];
  nextSaveEl = null;
  lastSyncedEl = null;
  root.dataset.automationId = rootId;
  root.style.minHeight = "100vh";
  root.style.padding = "";
  root.style.overflow = "";
  root.style.boxSizing = "";
}

// Pause detail for the status line: "paused — 14 min left" while a bounded pause
// counts down, plain "paused" otherwise (indefinite, or no detail yet).
function pauseRemainingLabel(secs: number): string {
  const mins = Math.floor(secs / 60);
  if (mins <= 0) {
    return t("windows.ui.lessthanaminuteleft");
  }
  if (mins < 60) {
    return t("windows.ui.arg0minleft", { "arg0": String(mins) });
  }
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  return m === 0 ? t("windows.ui.arg0hrleft", { "arg0": String(h) }) : t("windows.ui.hrlefthrminleftboth", { "count": Number(h), "count2": Number(m) });
}

function pausedStatusText(secs: number): string {
  return t("windows.ui.paused", { "count": String(pauseRemainingLabel(secs)) });
}

function statusStateLabel(dump: HealthDump): string {
  if (dump.app_state === "paused") {
    const secs = dump.pause?.seconds_remaining;
    return secs != null ? pausedStatusText(secs) : t("windows.ui.pausedxa7a9dc5b");
  }
  return phaseLabel(dump.app_state);
}

// Capture a status pill's text node so the 1 s timer can advance its bounded
// pause countdown in place. No-op unless this pill shows a bounded pause.
function trackPausePill(dump: HealthDump, chip: HTMLElement): void {
  if (dump.app_state === "paused" && dump.pause?.seconds_remaining != null) {
    const textEl = chip.firstElementChild;
    if (textEl instanceof HTMLElement) {
      pauseStatusEls.push(textEl);
    }
  }
}

// ── Global pause/resume hotkey (observer-hotkey) ──────────────────────────────
// A configurable global shortcut that toggles pause/resume. The owner picks
// modifiers + a key; the backend registers it with the OS and reports the honest
// outcome — a combo another app already owns shows as taken, never a silent no-op.
interface HotkeyConfig {
  enabled: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
  vk: number;
}
type HotkeyRegistration = "inactive" | "registered" | "combo_taken" | "failed";
interface HotkeyView {
  config: HotkeyConfig;
  registration: HotkeyRegistration;
}

// ── Microphone controls (observer-mic) ───────────────────────────────────────
// Device priority + per-device disable + input gain. The owner reorders/disables
// devices and sets gain; the capture loop opens the selected device and reports
// the actually-open id back as active_id, so "active" is earned, not guessed.
interface MicDeviceRef {
  id: string;
  name: string;
}
interface MicConfig {
  priority: string[];
  disabled: string[];
  gain: number;
}
interface MicView {
  config: MicConfig;
  active_id: string | null;
}
const MIC_GAIN_LEVELS = [1, 2, 4, 8];

// VK options the owner can pick as the main key (the backend validates the combo).
const HOTKEY_KEYS: ReadonlyArray<readonly [number, string]> = (() => {
  const out: Array<[number, string]> = [];
  for (let v = 0x41; v <= 0x5a; v++) {
    out.push([v, String.fromCharCode(v)]); // A-Z
  }
  for (let v = 0x30; v <= 0x39; v++) {
    out.push([v, String.fromCharCode(v)]); // 0-9
  }
  for (let n = 1; n <= 12; n++) {
    out.push([0x70 + n - 1, `F${n}`]); // F1-F12
  }
  out.push([0x20, "Space"]);
  return out;
})();

function hotkeyHasCombo(c: HotkeyConfig): boolean {
  return c.vk !== 0 && (c.ctrl || c.alt || c.shift || c.win);
}

function hotkeyStatusLabel(view: HotkeyView): string {
  switch (view.registration) {
    case "registered":
      return t("windows.ui.activex96879611");
    case "combo_taken":
      return t("windows.ui.thatcomboisinusebyanother");
    case "failed":
      return t("windows.ui.couldntregisterthatshortcut");
    case "inactive":
      if (!hotkeyHasCombo(view.config)) {
        return t("windows.ui.noshortcutset");
      }
      return view.config.enabled ? t("windows.tray.starting") : t("windows.ui.shortcutsetbutturnedoff");
  }
}

// Honest registration status as [text, color]: semantic color where the job is
// distinguishing states (registered = active, taken/failed = a problem), AA on
// the light ground. The text is the same source of truth as hotkeyStatusLabel.
function hotkeyStatusDisplay(view: HotkeyView): [string, string] {
  const labelText = hotkeyStatusLabel(view);
  switch (view.registration) {
    case "registered":
      return [labelText, "var(--accent)"];
    case "combo_taken":
    case "failed":
      return [labelText, "var(--danger)"];
    case "inactive":
      return [labelText, "var(--fg-subtle)"];
  }
}

function vkLabel(vk: number): string {
  const found = HOTKEY_KEYS.find(([v]) => v === vk);
  return found ? found[1] : "";
}

// "Ctrl + Alt + P" from a config; "" when there's no real combo to show.
function hotkeyComboString(cfg: HotkeyConfig): string {
  const parts: string[] = [];
  if (cfg.ctrl) parts.push("Ctrl");
  if (cfg.alt) parts.push("Alt");
  if (cfg.shift) parts.push("Shift");
  if (cfg.win) parts.push("Win");
  const key = vkLabel(cfg.vk);
  if (key) parts.push(key);
  return parts.join(" + ");
}

// Map a keydown to one of the allowed virtual-key codes (A–Z, 0–9, F1–F12,
// Space) — exactly the HOTKEY_KEYS set. Returns 0 for a modifier-only or
// unsupported key, so the capture keeps waiting for a real key.
function vkFromKeydown(event: KeyboardEvent): number {
  const code = event.code;
  if (code.length === 4 && code.startsWith("Key")) {
    return code.charCodeAt(3); // "KeyA" -> 'A' = 0x41
  }
  if (code.length === 6 && code.startsWith("Digit")) {
    return 0x30 + Number(code.slice(5));
  }
  if (/^F([1-9]|1[0-2])$/.test(code)) {
    return 0x70 + Number(code.slice(1)) - 1;
  }
  if (code === "Space") {
    return 0x20;
  }
  return 0;
}

function onHotkeyCaptureKey(event: KeyboardEvent): void {
  if (!hotkeyCapturing) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  if (event.key === "Escape") {
    stopHotkeyCapture();
    return;
  }
  const vk = vkFromKeydown(event);
  if (vk === 0) {
    return; // modifier-only or unsupported — wait for a real key.
  }
  const ctrl = event.ctrlKey;
  const alt = event.altKey;
  const shift = event.shiftKey;
  const win = event.metaKey;
  if (!(ctrl || alt || shift || win)) {
    return; // a global shortcut needs at least one modifier — keep waiting.
  }
  hotkeyCapturing = false;
  window.removeEventListener("keydown", onHotkeyCaptureKey, true);
  // The backend reports the honest registration outcome (registered / taken /
  // failed); the capture only records the owner's intended combo.
  void applyHotkey({ enabled: true, ctrl, alt, shift, win, vk });
}

function startHotkeyCapture(): void {
  if (hotkeyCapturing) {
    return;
  }
  hotkeyCapturing = true;
  window.addEventListener("keydown", onHotkeyCaptureKey, true);
  rerender();
}

function stopHotkeyCapture(): void {
  if (!hotkeyCapturing) {
    return;
  }
  hotkeyCapturing = false;
  window.removeEventListener("keydown", onHotkeyCaptureKey, true);
  rerender();
}

async function applyHotkey(next: HotkeyConfig): Promise<void> {
  latestHotkey = { config: next, registration: latestHotkey?.registration ?? "inactive" };
  rerender();
  try {
    await invoke("set_hotkey", { config: next });
  } catch {
    // Persistence failures are logged backend-side; the desired config still took.
  }
  // The pump reconciles registration within a poll (~250ms); refetch the honest
  // outcome shortly after so the status reflects registered / combo-taken.
  setTimeout(() => {
    void invoke<HotkeyView>("get_hotkey")
      .then((v) => {
        latestHotkey = v;
        rerender();
      })
      .catch(() => {});
  }, 450);
}

function renderHotkeySection(view: HotkeyView): HTMLElement {
  const pane = section(t("windows.ui.globalshortcut"));
  const cfg = view.config;

  pane.append(helpCaption(t("windows.ui.aglobalshortcuttopauseandresume")));

  pane.append(
    toggleRow(
      t("windows.ui.enablethepauseresumeshortcut"),
      ids["settings.hotkey.enabled"],
      cfg.enabled,
      (on) => {
        void applyHotkey({ ...cfg, enabled: on });
      },
    ),
  );

  // Press-to-capture — the native global-shortcut pattern. The owner clicks the
  // field and presses their combo; the field carries the settings.hotkey.combo
  // contract id (the combo control). Writes the same HotkeyConfig fields.
  pane.append(subheadLabel(t("windows.ui.yourshortcut")));
  const combo = hotkeyComboString(cfg);
  const field = document.createElement("button");
  field.dataset.automationId = ids["settings.hotkey.combo"];
  field.classList.add("fluent-control");
  field.style.display = "flex";
  field.style.alignItems = "center";
  field.style.justifyContent = "space-between";
  field.style.gap = "10px";
  field.style.width = "100%";
  field.style.textAlign = "left";
  field.style.fontSize = "13px";
  field.style.padding = "9px 12px";
  field.style.borderRadius = "var(--radius-control)";
  field.style.border = `1px dashed ${hotkeyCapturing ? "var(--accent)" : "var(--border)"}`;
  field.style.background = hotkeyCapturing ? "var(--accent-subtle)" : "var(--bg-input)";
  field.style.color = "var(--fg)";
  field.style.cursor = "pointer";
  if (hotkeyCapturing) {
    const lead = text("span", t("windows.ui.pressyourshortcut"));
    lead.style.color = "var(--accent)";
    lead.style.fontWeight = "600";
    const hint = text("span", t("windows.ui.esctocancel"));
    hint.style.color = "var(--muted)";
    hint.style.fontSize = "12px";
    field.append(lead, hint);
    field.onclick = () => stopHotkeyCapture();
  } else if (combo) {
    const kbd = text("span", combo);
    kbd.style.fontFamily = 'ui-monospace, "Cascadia Code", Consolas, monospace';
    const hint = text("span", t("windows.ui.change"));
    hint.style.color = "var(--fg-subtle)";
    hint.style.fontSize = "12px";
    field.append(kbd, hint);
    field.onclick = () => startHotkeyCapture();
  } else {
    const lead = text("span", t("windows.ui.clickthenpressyourshortcut"));
    lead.style.color = "var(--fg-subtle)";
    const hint = text("span", t("windows.ui.set"));
    hint.style.color = "var(--accent)";
    hint.style.fontSize = "12px";
    hint.style.fontWeight = "600";
    field.append(lead, hint);
    field.onclick = () => startHotkeyCapture();
  }
  pane.append(field);
  pane.append(microCaption(t("windows.ui.holdoneormoreofctrlalt")));

  const [statusText, statusColor] = hotkeyStatusDisplay(view);
  const statusEl = automation(text("div", statusText), ids["settings.hotkey.status"]);
  statusEl.style.color = statusColor;
  pane.append(valueRow(t("windows.ui.status"), statusEl));

  // Clear — a quiet text button, present only when a combo is set.
  if (hotkeyHasCombo(cfg)) {
    const clear = document.createElement("button");
    clear.textContent = t("windows.ui.textcontent4");
    clear.dataset.automationId = ids["settings.hotkey.clear"];
    clear.style.border = "none";
    clear.style.background = "transparent";
    clear.style.color = "var(--fg-subtle)";
    clear.style.cursor = "pointer";
    clear.style.fontSize = "12px";
    clear.style.padding = "6px 0";
    clear.style.textDecoration = "underline";
    clear.onclick = () => {
      stopHotkeyCapture();
      void applyHotkey({
        enabled: false,
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        vk: 0,
      });
    };
    const clearWrap = document.createElement("div");
    clearWrap.append(clear);
    pane.append(clearWrap);
  }

  return pane;
}

// Display order: priority ids first (those present), then remaining present
// devices in enumeration order. Reorder/disable operate on this list.
function orderedMicDevices(cfg: MicConfig, devices: MicDeviceRef[]): MicDeviceRef[] {
  const byId = new Map(devices.map((d) => [d.id, d]));
  const out: MicDeviceRef[] = [];
  const seen = new Set<string>();
  for (const id of cfg.priority) {
    const d = byId.get(id);
    if (d && !seen.has(id)) {
      out.push(d);
      seen.add(id);
    }
  }
  for (const d of devices) {
    if (!seen.has(d.id)) {
      out.push(d);
      seen.add(d.id);
    }
  }
  return out;
}

async function applyMic(next: MicConfig): Promise<void> {
  latestMic = { config: next, active_id: latestMic?.active_id ?? null };
  rerender();
  try {
    await invoke("set_mic_config", { config: next });
  } catch {
    // Persistence failures are logged backend-side; the desired config still took.
  }
  // The capture loop reconciles selection within ~1s; refetch the active device.
  setTimeout(() => {
    void invoke<MicView>("get_mic_config")
      .then((v) => {
        latestMic = v;
        rerender();
      })
      .catch(() => {});
  }, 1200);
}

function reorderButton(
  glyph: string,
  enabled: boolean,
  ariaLabel: string,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = glyph;
  b.disabled = !enabled;
  b.setAttribute("aria-label", ariaLabel);
  b.classList.add("fluent-control");
  b.style.fontSize = "13px";
  b.style.lineHeight = "1";
  b.style.padding = "3px 7px";
  b.style.border = "1px solid var(--border)";
  b.style.borderRadius = "var(--radius-control)";
  b.style.background = "var(--fill)";
  b.style.color = enabled ? "var(--fg-subtle)" : "var(--muted)";
  b.style.cursor = enabled ? "pointer" : "default";
  if (enabled) {
    b.onclick = onClick;
  }
  return b;
}

function renderMicSection(view: MicView): HTMLElement {
  const pane = section(t("windows.ui.microphones"));
  const cfg = view.config;
  const ordered = orderedMicDevices(cfg, micDevices);

  pane.append(
    helpCaption(
      t("windows.ui.thesolstoneappusesonemicrophoneat"),
    ),
  );

  // Device priority first (macOS-sibling order) — reorder + enable/disable.
  pane.append(subheadLabel(t("windows.ui.devicepriority")));
  pane.append(
    helpCaption(
      t("windows.ui.thetopenabledmicrophoneisuseduse"),
    ),
  );
  const list = automation(document.createElement("div"), ids["settings.mic.devices"]);
  list.style.display = "flex";
  list.style.flexDirection = "column";
  list.style.gap = "6px";
  list.style.padding = "4px 0";
  if (ordered.length === 0) {
    const empty = text("div", t("windows.ui.nomicrophoneinputdevices"));
    empty.style.color = "var(--muted)";
    empty.style.fontSize = "13px";
    list.append(empty);
  }
  ordered.forEach((d, idx) => {
    const disabled = cfg.disabled.includes(d.id);
    const row = document.createElement("div");
    row.style.display = "flex";
    row.style.alignItems = "center";
    row.style.gap = "8px";
    row.style.fontSize = "13px";

    const orderIds = ordered.map((x) => x.id);
    row.append(
      reorderButton("↑", idx > 0, t("windows.ui.moveup", { "name": String(d.name) }), () => {
        const o = [...orderIds];
        [o[idx - 1], o[idx]] = [o[idx], o[idx - 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );
    row.append(
      reorderButton("↓", idx < ordered.length - 1, t("windows.ui.movedown", { "name": String(d.name) }), () => {
        const o = [...orderIds];
        [o[idx + 1], o[idx]] = [o[idx], o[idx + 1]];
        void applyMic({ ...cfg, priority: o });
      }),
    );

    const label = text("span", d.name);
    label.style.flex = "1";
    if (disabled) {
      label.style.textDecoration = "line-through";
      label.style.color = "var(--muted)";
    }
    if (d.id === view.active_id) {
      const badge = text("span", t("windows.ui.activex68f50a9b"));
      badge.style.color = "var(--accent)";
      label.append(badge);
    }
    row.append(label);

    const toggle = document.createElement("button");
    toggle.textContent = disabled ? t("windows.ui.enable") : t("windows.ui.disable");
    toggle.classList.add("fluent-control");
    toggle.style.fontSize = "12px";
    toggle.style.padding = "4px 10px";
    toggle.style.border = "1px solid var(--border)";
    toggle.style.borderRadius = "var(--radius-control)";
    toggle.style.background = "var(--fill)";
    toggle.style.color = "var(--fg-subtle)";
    toggle.style.cursor = "pointer";
    toggle.onclick = () => {
      const next = disabled
        ? cfg.disabled.filter((x) => x !== d.id)
        : [...cfg.disabled, d.id];
      void applyMic({ ...cfg, disabled: next });
    };
    row.append(toggle);

    list.append(row);
  });
  pane.append(list);

  // Active device — earned from what the loop actually opened (never guessed).
  // Carries the settings.mic.active contract id.
  const activeName =
    ordered.find((d) => d.id === view.active_id)?.name ??
    (view.active_id ? t("windows.ui.selecteddevice") : t("windows.ui.nonedisplay"));
  const activeRow = document.createElement("div");
  activeRow.style.display = "flex";
  activeRow.style.gap = "6px";
  activeRow.style.fontSize = "12px";
  activeRow.style.color = "var(--fg-subtle)";
  activeRow.style.margin = "6px 0 0";
  const activeVal = automation(text("span", activeName), ids["settings.mic.active"]);
  activeVal.style.color = "var(--fg)";
  activeRow.append(text("span", t("windows.ui.activex474d1b75")), activeVal);
  pane.append(activeRow);

  // Input gain — segmented 1× / 2× / 4× / 8×.
  pane.append(subheadLabel(t("windows.ui.inputgain")));
  const gainRow = automation(document.createElement("div"), ids["settings.mic.gain"]);
  gainRow.setAttribute("role", "group");
  gainRow.setAttribute("aria-label", t("windows.ui.inputgain"));
  gainRow.style.display = "flex";
  gainRow.style.gap = "6px";
  for (const level of MIC_GAIN_LEVELS) {
    const on = cfg.gain === level;
    const b = document.createElement("button");
    b.textContent = t("windows.ui.textcontent5", { "level": String(level) });
    b.classList.add(on ? "fluent-accent" : "fluent-control");
    b.style.fontSize = "13px";
    b.style.padding = "5px 12px";
    b.style.border = on ? "1px solid var(--accent)" : "1px solid var(--border)";
    b.style.borderRadius = "var(--radius-control)";
    b.style.background = on ? "var(--accent)" : "var(--fill)";
    b.style.color = on ? "var(--accent-fg)" : "var(--fg-subtle)";
    b.style.cursor = "pointer";
    b.onclick = () => {
      void applyMic({ ...cfg, gain: level });
    };
    gainRow.append(b);
  }
  pane.append(gainRow);
  pane.append(
    microCaption(
      t("windows.ui.alouderinputforquietmicrophoneschanges"),
    ),
  );

  return pane;
}

function routeLabel(route: Route): string {
  return ROUTES.find((item) => item.route === route)?.label ?? route;
}

function navigateTo(route: Route): void {
  if (latestHealth && showsDesktopShell(latestHealth)) {
    const mapped = legacyRoute(route);
    deskRow = mapped.row;
    deskPane = mapped.pane;
    deskPhonePane = mapped.row !== "home" || mapped.pane !== "status";
    closeProofConsumer();
    focusPaneTitleOnRender = true;
    closeNavOverlay();
    rerender();
    return;
  }
  activeRoute = route;
  closeProofConsumer();
  focusPaneTitleOnRender = true;
  closeNavOverlay();
  rerender();
}

function loadingCaption(): HTMLElement {
  const cap = helpCaption(t("windows.ui.notavailablerightnow"));
  cap.style.padding = "18px 20px";
  return cap;
}

function clearLastSectionDivider(container: HTMLElement): void {
  const sections = Array.from(container.children).filter(
    (child): child is HTMLElement =>
      child instanceof HTMLElement && child.tagName.toLowerCase() === "section",
  );
  if (sections.length === 0) {
    return;
  }
  sections[sections.length - 1].style.borderBottom = "none";
}

const BROWSER_COPY = {
  connected: t("windows.desktop.shared.copy.sourcesbrowserconnectednow"),
  none: t("windows.ui.nobrowserconnectednow"),
  paused: t("windows.ui.pausedwiththerestofthesolstone"),
  held: t("windows.desktop.shared.copy.sourcesbrowserheld"),
  full: t("windows.ui.noroommorerightnownew"),
  notPaired: t("windows.ui.pairwithyourjournalfirst"),
  setup:
    t("windows.ui.ifyouhaventyetaddthe"),
  failed: t("windows.ui.keptonthispcnotreachingyour"),
  discard: t("windows.desktop.shared.copy.sourcesbrowserdiscard"),
  discardConfirm: t("windows.desktop.shared.copy.sourcesbrowserdiscardconfirm"),
  discardConfirmButton: t("windows.desktop.shared.copy.sourcesbrowserdiscardcommit"),
  cancel: "cancel",
  discarded: "discarded",
  discardFailed: t("windows.ui.couldntfinishdiscardingwhatsleft"),
} as const;

const BROWSER_NAMES: Record<string, string> = { chrome: "Chrome", edge: "Edge", firefox: "Firefox" };

function browserStateText(status: BrowserStatus): string {
  // The owner's own move first, then what the app is holding.
  if (status.capture === "not_paired") {
    return BROWSER_COPY.notPaired;
  }
  if (status.capture === "paused") {
    return BROWSER_COPY.paused;
  }
  if (status.custody.full) {
    return status.delivery === "failed"
      ? t("windows.tray.text", { "tooltip": String(BROWSER_COPY.full), "notice": String(BROWSER_COPY.failed) })
      : BROWSER_COPY.full;
  }
  if (status.delivery === "failed" && status.custody.held_bytes > 0) {
    return BROWSER_COPY.failed;
  }
  switch (status.capture) {
    case "permitted": {
      if (status.connected.length === 0) {
        return BROWSER_COPY.none;
      }
      const names = [...new Set(status.connected.map((c) => BROWSER_NAMES[c.brand] ?? c.brand))];
      return t("windows.desktop.shared.copy.text", { "value": String(BROWSER_COPY.connected), "value2": String(names.join(", ")) });
    }
    case "intake_off":
    case "unavailable":
      return BROWSER_COPY.held;
  }
}

function quietButton(labelText: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.textContent = labelText;
  b.style.border = "none";
  b.style.background = "transparent";
  b.style.color = "var(--fg-subtle)";
  b.style.cursor = "pointer";
  b.style.fontSize = "12px";
  b.style.padding = "6px 12px 6px 0";
  b.style.textDecoration = "underline";
  b.onclick = onClick;
  return b;
}

async function discardWaitingBrowserPages(): Promise<void> {
  browserDiscard = "busy";
  requestRerender();
  const result = await invoke<{ left: number } | null>("browser_discard_waiting").catch(() => null);
  browserDiscard = result !== null && result.left === 0 ? "done" : "failed";
  latestBrowser = await invoke<BrowserStatus | null>("browser_status").catch(() => latestBrowser);
  requestRerender();
}

async function openBrowserStore(brand: string): Promise<void> {
  try {
    const ok = await invoke<boolean>("open_browser_store", { brand });
    if (ok) {
      browserStoreFailure = null;
    } else {
      browserStoreFailure = brand;
    }
  } catch {
    browserStoreFailure = brand;
  }
  requestRerender();
}

function renderBrowserRows(status: BrowserStatus): HTMLElement[] {
  const rows: HTMLElement[] = [valueRow(t("windows.desktop.shared.copy.sourcesbrowserpages"), selectable(text("div", browserStateText(status))))];
  const offerVisible = status.connected.length === 0 && status.capture !== "not_paired";
  if (!offerVisible) {
    browserStoreFailure = null;
  } else {
    const block = document.createElement("div");
    const guidance = text("div", BROWSER_COPY.setup);
    automation(guidance, ids["settings.sources.browser.offer"]);
    const storesContainer = document.createElement("div");
    automation(storesContainer, ids["settings.sources.browser.stores"]);
    for (const brand of status.stores) {
      const btn = quietButton(t("windows.ui.addto", { "value": String(BROWSER_NAMES[brand] ?? brand) }), () => {
        void openBrowserStore(brand);
      });
      btn.dataset.brand = brand;
      storesContainer.append(btn);
    }
    block.append(guidance, storesContainer);
    if (browserStoreFailure !== null) {
      const err = text(
        "div",
        t("windows.ui.couldntopenthestorepagein", { "value": String(BROWSER_NAMES[browserStoreFailure] ?? browserStoreFailure) }),
      );
      automation(err, ids["settings.sources.browser.storeError"]);
      err.style.color = "var(--danger)";
      block.append(err);
    }
    rows.push(valueRow("", block));
  }
  const waiting = status.custody.waiting;
  if (!waiting && browserDiscard !== "done" && browserDiscard !== "failed") {
    return rows;
  }
  const box = document.createElement("div");
  const actions = document.createElement("div");
  if (browserDiscard === "confirm") {
    box.append(text("div", BROWSER_COPY.discardConfirm));
    actions.append(
      quietButton(BROWSER_COPY.discardConfirmButton, () => void discardWaitingBrowserPages()),
      quietButton(BROWSER_COPY.cancel, () => {
        browserDiscard = "idle";
        requestRerender();
      }),
    );
  } else if (browserDiscard === "busy") {
    const busy = quietButton(BROWSER_COPY.discard, () => {});
    busy.disabled = true;
    busy.setAttribute("aria-busy", "true");
    actions.append(busy);
  } else if (waiting) {
    actions.append(
      quietButton(BROWSER_COPY.discard, () => {
        browserDiscard = "confirm";
        requestRerender();
      }),
    );
  }
  if (browserDiscard === "done" && !waiting) {
    box.append(text("div", BROWSER_COPY.discarded));
  } else if (browserDiscard === "failed") {
    const failed = text("div", BROWSER_COPY.discardFailed);
    failed.style.color = "var(--danger)";
    box.append(failed);
  }
  box.append(actions);
  rows.push(valueRow("", box));
  return rows;
}

const OBSIDIAN_COPY = {
  label: "Obsidian",
  explain:
    t("windows.ui.solstoneapptakesnotesonevault"),
  foundLabel: t("windows.ui.vaultscomputer"),
  noneFound: t("windows.ui.solstoneappdidntfindvault"),
  use: t("windows.ui.usevault"),
  chooseFolder: t("windows.ui.choosefolder"),
  refusedNotVault: t("windows.ui.thatfolderisntobsidianvault"),
  refusedName: t("windows.ui.thatfoldersnamecant"),
  couldNotTurnOn: t("windows.ui.couldntturntryagain"),
  checking: t("windows.ui.waitingreachjournal"),
  waitingUpdate: t("windows.ui.waitingjournalupdate"),
  waitingUpdateDetail: t("windows.ui.journalneedsupdatebeforecantake"),
  on: "on",
  paused: "paused",
  needsAttention: t("windows.desktop.shared.copy.settingstabattentiona11y"),
  vault: "vault",
  lastChecked: t("windows.ui.lastchecked"),
  notOnDevice: t("windows.ui.notonthiscomputer"),
  tooLarge: t("windows.ui.toolargebring"),
  attentionVault:
    t("windows.ui.solstoneappcantreadvault"),
  attentionPart: t("windows.ui.partvaultcantberead"),
  attentionFile: t("windows.ui.somenotescouldntgointo"),
  attentionStorage: t("windows.ui.solstoneappcouldntsaveits"),
  warningHeading: t("windows.ui.vaultwithnamealreadyjournal"),
  waitingAnswer: t("windows.ui.waitingforyouranswer"),
  continue: "continue",
  cancel: "cancel",
  turnOff: t("windows.ui.turnoff"),
  turnOffNote:
    t("windows.ui.turningoffstopsbringingchangeswhat"),
} as const;

/** Every screenshots string, by its id in the locked owner copy. */
const SCREENSHOTS_COPY = {
  S1: t("windows.ui.screenshots"),
  S4: (folder: string) => t("windows.ui.from", { "value": String(folder) }),
  S5: (done: number, total: number) => t("windows.ui.arg0ofarg1inyourjournal", { "arg0": String(done), "arg1": String(total) }),
  S6: (done: number, total: number) => t("windows.ui.arg0ofarg1screenshotsinyourjournal", { "arg0": String(done), "arg1": String(total) }),
  S7: t("windows.ui.frombeforetoday"),
  S8: {
    thirty_days: t("windows.ui.thelast30days"),
    six_months: t("windows.ui.thelast6months"),
    year: t("windows.ui.thelastyear"),
    all: t("windows.ui.allofit"),
    nothing: t("windows.ui.nothingfrombeforetoday"),
  } as Record<ScreenshotsSpan, string>,
  S9: t("windows.ui.youllseehowmanyscreenshotstherearebeforeanycomei"),
  S10: (count: number) => (count === 1 ? t("windows.ui.bringin1screenshot") : t("windows.ui.bringinarg0screenshots", { "arg0": String(count.toLocaleString("en-US")) })),
  S10Today: t("windows.ui.startfromtoday"),
  checking: t("windows.ui.checking"),
  S11: t("windows.ui.choosehowfarbacktostartthenbringthemin"),
  S12: t("windows.ui.couldntcountyourscreenshots"),
  S12Action: t("windows.desktop.shared.copy.tryagain"),
  S14: t("windows.ui.ascreenshotalreadyinyourjournalstaysthereifyoude"),
  S15: t("windows.ui.whatsalreadyinyourjournalstayswhenyoupauseorturn"),
  /** S16's second sentence: under the computer's whole pause it stands alone. */
  S16ComputerPause: t("windows.ui.screenshotsyoutakewhileitspauseddontcomein"),
  S17: t("windows.ui.screenshotsalreadyinyourjournalarentbroughtinaga"),
  S18: t("windows.ui.lastchecked"),
  S19: t("windows.ui.waitingtogotoyourjournal"),
  S20: (count: number) => quantity("screenshots", count),
  S21: t("windows.ui.waitingforajournalupdateyourscreenshotswaitonthi"),
  S22: t("windows.ui.folder"),
  S23: t("windows.ui.onlywhatthesolstoneappcantellisascreenshotcomesi"),
  D1: t("windows.ui.whilethisisonthesolstoneapptakesinthescreenshots"),
  D3: (folder: string) => t("windows.ui.otherfilesinarg0staywheretheyare", { "arg0": String(folder) }),
  D8: t("windows.ui.couldntfindascreenshotsfolderonthispcchoosethefo"),
  D9: (folder: string) => t("windows.ui.thearg0folderisntonthispcanymore", { "arg0": String(folder) }),
  D12: (count: number) =>
    count === 1
      ? t("windows.ui.value1screenshotcantcomeinsoitstaysinthefolder")
      : t("windows.ui.arg0screenshotscantcomeinsotheystayinthefolder", { "arg0": String(count) }),
  D13: (count: number) => (count === 1 ? t("windows.ui.couldntread1screenshot") : t("windows.ui.couldntreadarg0screenshots", { "arg0": String(count) })),
  D13Action: t("windows.desktop.shared.copy.tryagain"),
  D15: t("windows.ui.moreaboutthis"),
  D16: (folder: string) => t("windows.ui.everypicturesavedinarg0comesinnotonlyyourscreens", { "arg0": String(folder) }),
  D17: t("windows.ui.ascreenshotthatsonlycopiedtotheclipboardisntsave"),
  chooseFolder: t("windows.ui.chooseafolderxdbec742d"),
  resume: t("windows.desktop.shared.setup.wizardpausefailure2"),
} as const;

function obsidianWarningText(warning: NonNullable<ObsidianStatus["warning"]>): string {
  const parsed = warning.last_synced_at ? new Date(warning.last_synced_at) : null;
  const date = parsed && !Number.isNaN(parsed.getTime()) ? parsed.toLocaleDateString(formatLocale, {year:"numeric",month:"short",day:"numeric"}) : null;
  if (warning.device && date) return t("windows.obsidian.warningdevicedate", { vault: String(warning.vault), device: String(warning.device), date: String(date) });
  if (warning.device) return t("windows.obsidian.warningdevice", { vault: String(warning.vault), device: String(warning.device) });
  if (date) return t("windows.obsidian.warningdate", { vault: String(warning.vault), date: String(date) });
  return t("windows.obsidian.warning", { vault: String(warning.vault) });
}

function obsidianProgressText(sync: NonNullable<ObsidianStatus["first_sync"]>): string {
  return t("windows.ui.notesjournal", { "value": String(sync.sent), "value2": String(sync.found) });
}

/** The tile line while Obsidian sets up: what it is waiting on, never a generic one. */
function obsidianSettingUpLine(status: ObsidianStatus | null): string | undefined {
  if (status?.state === "confirm" && status.warning) return OBSIDIAN_COPY.waitingAnswer;
  if (status?.state === "waiting_for_journal_update") return OBSIDIAN_COPY.waitingUpdate;
  return undefined;
}

/** What the Obsidian page shows once a vault is chosen: progress, what waits, and what stays out. */
function obsidianDeskStatus(status: ObsidianStatus): HTMLElement | null {
  if (status.state === "off" || status.state === "confirm") return null;
  const block = document.createElement("div");
  block.className = "desk-facts";
  const fact = (label: string, value: string): void => {
    const row = document.createElement("div");
    row.className = "desk-fact";
    const name = text("span", label);
    name.className = "desk-fact-label";
    const shown = text("span", value);
    shown.className = "desk-fact-value";
    row.append(name, shown);
    block.append(row);
  };
  if (status.vault) fact(OBSIDIAN_COPY.vault, status.vault);
  fact(OBSIDIAN_COPY.lastChecked, lastSyncedLabel(status.last_checked_ms, nowSecs()));
  if (status.not_on_device > 0) fact(OBSIDIAN_COPY.notOnDevice, String(status.not_on_device));
  if (status.too_large > 0) fact(OBSIDIAN_COPY.tooLarge, String(status.too_large));
  return block;
}

function obsidianStateText(state: ObsidianStateName): string {
  switch (state) {
    case "checking":
      return OBSIDIAN_COPY.checking;
    case "confirm":
      return "";
    case "on":
      return OBSIDIAN_COPY.on;
    case "paused":
      return OBSIDIAN_COPY.paused;
    case "waiting_for_journal_update":
      return OBSIDIAN_COPY.waitingUpdate;
    case "needs_attention":
      return OBSIDIAN_COPY.needsAttention;
    case "off":
      return t("windows.desktop.shared.app.microphonemodeoff");
  }
}

function obsidianAttentionText(attention: ObsidianAttention): string {
  switch (attention) {
    case "vault_unreadable":
      return OBSIDIAN_COPY.attentionVault;
    case "part_unreadable":
      return OBSIDIAN_COPY.attentionPart;
    case "file_refused":
      return OBSIDIAN_COPY.attentionFile;
    case "storage":
      return OBSIDIAN_COPY.attentionStorage;
  }
}

function obsidianRefusalText(refusal: ObsidianRefusal): string {
  return refusal === "not_a_vault" ? OBSIDIAN_COPY.refusedNotVault : OBSIDIAN_COPY.refusedName;
}

async function refreshObsidianFound(): Promise<void> {
  const listed = await invoke<ObsidianVault[] | null>("obsidian_found_vaults").catch(() => null);
  obsidianFound = listed ?? [];
  requestRerender();
}

async function applyObsidianDocument(run: Promise<ObsidianDocument | null>): Promise<void> {
  obsidianBusy = true;
  obsidianFailed = false;
  requestRerender();
  try {
    const document = await run;
    if (document) {
      latestObsidian = document.status;
      obsidianRefusal = document.refusal ?? null;
    }
  } catch {
    obsidianFailed = true;
  }
  obsidianBusy = false;
  await refreshObsidianFound();
}

function renderObsidianRows(status: ObsidianStatus): HTMLElement[] {
  const stateValue = selectable(automation(text("div", obsidianStateText(status.state)), ids["settings.sources.obsidian.state"]));
  const rows: HTMLElement[] = [valueRow(OBSIDIAN_COPY.label, stateValue)];

  if (status.state === "off") {
    const block = document.createElement("div");
    const explain = text("div", OBSIDIAN_COPY.explain);
    explain.style.margin = "0 0 8px";
    block.append(explain);
    const found = document.createElement("div");
    automation(found, ids["settings.sources.obsidian.found"]);
    if (obsidianFound.length > 0) {
      found.append(text("div", OBSIDIAN_COPY.foundLabel));
      for (const vault of obsidianFound) {
        const row = document.createElement("div");
        row.style.display = "flex";
        row.style.alignItems = "center";
        row.style.gap = "10px";
        row.style.padding = "4px 0";
        row.append(selectable(text("div", vault.name)));
        const use = actionButton(OBSIDIAN_COPY.use, undefined, !obsidianBusy, () => {
          obsidianRefusal = null;
          void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_use_found_vault", { path: vault.path }));
        }, obsidianBusy, "standard");
        row.append(use);
        found.append(row);
      }
    } else {
      found.append(text("div", OBSIDIAN_COPY.noneFound));
    }
    block.append(found);
    const choose = actionButton(OBSIDIAN_COPY.chooseFolder, ids["settings.sources.obsidian.choose"], !obsidianBusy, () => {
      obsidianRefusal = null;
      void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_choose_folder"));
    }, obsidianBusy, "standard");
    const actions = document.createElement("div");
    actions.style.marginTop = "8px";
    actions.append(choose);
    block.append(actions);
    if (obsidianRefusal !== null || obsidianFailed) {
      const refusal = text("div", obsidianRefusal !== null ? obsidianRefusalText(obsidianRefusal) : OBSIDIAN_COPY.couldNotTurnOn);
      refusal.style.color = "var(--danger)";
      refusal.style.marginTop = "6px";
      block.append(automation(refusal, ids["settings.sources.obsidian.refusal"]));
    }
    rows.push(valueRow("", block));
    return rows;
  }

  if (status.state === "confirm" && status.warning) {
    const block = document.createElement("div");
    const heading = text("div", OBSIDIAN_COPY.warningHeading);
    heading.style.fontWeight = "600";
    heading.style.marginBottom = "4px";
    block.append(heading, automation(text("div", obsidianWarningText(status.warning)), ids["settings.sources.obsidian.warning"]));
    const actions = document.createElement("div");
    actions.style.display = "flex";
    actions.style.gap = "8px";
    actions.style.marginTop = "8px";
    actions.append(
      actionButton(OBSIDIAN_COPY.continue, ids["settings.sources.obsidian.continue"], !obsidianBusy, () => {
        void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_continue_warning"));
      }, obsidianBusy),
      actionButton(OBSIDIAN_COPY.cancel, ids["settings.sources.obsidian.cancel"], !obsidianBusy, () => {
        void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_cancel_warning"));
      }, obsidianBusy, "standard"),
    );
    block.append(actions);
    rows.push(valueRow("", block));
    return rows;
  }

  if (status.vault) {
    rows.push(valueRow(OBSIDIAN_COPY.vault, selectable(text("div", status.vault))));
  }
  if (status.state === "waiting_for_journal_update") {
    rows.push(valueRow("", text("div", OBSIDIAN_COPY.waitingUpdateDetail)));
  }
  if (status.first_sync) {
    rows.push(valueRow("", selectable(text("div", obsidianProgressText(status.first_sync)))));
  }
  if (status.waiting > 0) {
    rows.push(valueRow("", selectable(text("div", t("windows.ui.waitingsync", { "value": String(status.waiting) })))));
  }
  rows.push(valueRow(OBSIDIAN_COPY.lastChecked, selectable(text("div", lastSyncedLabel(status.last_checked_ms, nowSecs())))));
  if (status.not_on_device > 0) {
    rows.push(valueRow(OBSIDIAN_COPY.notOnDevice, selectable(text("div", String(status.not_on_device)))));
  }
  if (status.too_large > 0) {
    rows.push(valueRow(OBSIDIAN_COPY.tooLarge, selectable(text("div", String(status.too_large)))));
  }
  if (status.attention) {
    const attention = automation(text("div", obsidianAttentionText(status.attention)), ids["settings.sources.obsidian.attention"]);
    attention.style.color = "var(--danger)";
    rows.push(valueRow("", attention));
  }
  const offBlock = document.createElement("div");
  offBlock.append(
    actionButton(OBSIDIAN_COPY.turnOff, ids["settings.sources.obsidian.turnOff"], !obsidianBusy, () => {
      void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_turn_off"));
    }, obsidianBusy, "standard"),
  );
  const offNote = text("div", OBSIDIAN_COPY.turnOffNote);
  offNote.style.fontSize = "12px";
  offNote.style.color = "var(--fg-subtle)";
  offNote.style.marginTop = "6px";
  offBlock.append(offNote);
  rows.push(valueRow("", offBlock));
  return rows;
}

function renderSourceControl(
  kind: SourceKind,
  report: SourceReport | undefined,
  stateId: string,
  switchId: string,
  dump: HealthDump,
): HTMLElement {
  const wrap = document.createElement("div");
  wrap.style.display = "flex";
  wrap.style.alignItems = "center";
  wrap.style.gap = "10px";

  const enabled = isSourceEnabled(dump.setup, kind);
  // Desktop activity's switch is the owner's choice; whether it runs also follows screen.
  const switchOn = kind === "desktop_activity" ? dump.setup?.desktop_activity_allowed === true : enabled;
  const status = deriveSourceStatus(enabled, isSourcePaused(dump, kind), report);
  const failed = dump.setup?.failed_sources?.find((f) => f.kind === kind);

  const accessibleName =
    kind === "mic"
      ? t("windows.ui.microphone")
      : kind === "system_audio"
        ? t("windows.ui.computersound")
        : kind === "desktop_activity"
          ? t("windows.ui.desktopactivity")
          : t("windows.ui.screen");

  const toggle = toggleRow("", switchId, switchOn, (on) => {
    void invoke("set_source_enabled", { kind, enabled: on }).then(() => retryHealth());
  });
  const input = toggle.querySelector("input");
  if (input) {
    input.setAttribute("aria-label", accessibleName);
    if (status !== null) {
      input.dataset.status = status;
    }
  }
  if (status !== null) {
    toggle.dataset.status = status;
  }

  const needsPermission = isAccessDenied(report);
  const pillEl = selectable(automation(sourcePill(status, needsPermission), stateId));
  wrap.append(toggle, pillEl);
  if (status === "unavailable" && needsPermission && kind === "mic") {
    wrap.append(
      actionButton(t("windows.ui.openwindowssettings"), ids["settings.sources.mic.permission"], true, () =>
        void invoke("open_microphone_privacy_settings"),
      ),
    );
  }
  if (failed) {
    const failEl = text("div", sourceSaveFailureMessage(failed));
    failEl.classList.add("save-failure");
    failEl.style.flexBasis = "100%";
    wrap.style.flexWrap = "wrap";
    wrap.append(failEl);
  }
  return wrap;
}

const ACTIVITY_SENTENCE =
  t("windows.ui.addappnameswindowtitlesandpage");

function renderDesktopActivityControl(
  report: SourceReport | undefined,
  dump: HealthDump,
): HTMLElement {
  const container = document.createElement("div");
  const control = renderSourceControl(
    "desktop_activity",
    report,
    ids["settings.sources.desktopActivity.state"],
    ids["settings.sources.desktopActivity.enabled"],
    dump,
  );
  container.append(control);

  const sentence = text("div", ACTIVITY_SENTENCE);
  sentence.style.fontSize = "12px";
  sentence.style.color = "var(--fg-subtle)";
  sentence.style.margin = "8px 0 6px";
  sentence.style.lineHeight = "1.4";
  container.append(sentence);

  const titlesEnabled = dump.setup?.desktop_activity_window_titles ?? false;
  const addressesEnabled = dump.setup?.desktop_activity_page_addresses ?? false;

  const titlesToggle = toggleRow(
    t("windows.desktop.shared.settings.windowtitles"),
    ids["settings.sources.desktopActivity.windowTitles"],
    titlesEnabled,
    (on) => {
      void invoke("set_desktop_activity_window_titles", { enabled: on }).catch(() => undefined).then(() => retryHealth());
    },
  );
  titlesToggle.style.padding = "2px 0";

  const addressesToggle = toggleRow(
    t("windows.ui.pageaddressesedgeandchrome"),
    ids["settings.sources.desktopActivity.pageAddresses"],
    addressesEnabled,
    (on) => {
      void invoke("set_desktop_activity_page_addresses", { enabled: on }).catch(() => undefined).then(() => retryHealth());
    },
  );
  addressesToggle.style.padding = "2px 0";

  const throughPauseToggle = toggleRow(
    t("windows.ui.keeponwhilepausedorscreenis"),
    ids["settings.sources.desktopActivity.throughPause"],
    dump.setup?.desktop_activity_through_pause ?? false,
    (on) => {
      void invoke("set_desktop_activity_through_pause", { enabled: on }).catch(() => undefined).then(() => retryHealth());
    },
  );
  throughPauseToggle.style.padding = "2px 0";

  for (const [toggle, failed] of [
    [titlesToggle, dump.setup?.desktop_activity_window_titles_save_failed],
    [addressesToggle, dump.setup?.desktop_activity_page_addresses_save_failed],
    [throughPauseToggle, dump.setup?.desktop_activity_through_pause_save_failed],
  ] as const) {
    container.append(toggle);
    if (failed) {
      const failure = text("div", t("windows.ui.couldnotsavethischoiceitmay"));
      failure.classList.add("save-failure");
      container.append(failure);
    }
  }
  return container;
}
const savedChoicesUnread =
  t("windows.ui.couldntreadyoursavedsourcechoices");

function renderSourcesSection(dump: HealthDump): HTMLElement {
  if (dump.setup?.sequence_enabled !== true) {
    const sources = section(t("windows.desktop.shared.app.sourcestitle"));
    if (dump.setup?.setup_read_failed === true) {
      const failureNotice = text("div", savedChoicesUnread);
      failureNotice.classList.add("save-failure");
      sources.prepend(failureNotice);
    }
    for (const [kind, label, id] of [
      ["screen", t("windows.ui.screen"), ids["settings.sources.screen.state"]],
      ["system_audio", t("windows.ui.computersound"), ids["settings.sources.systemAudio.state"]],
      ["mic", t("windows.ui.microphone"), ids["settings.sources.mic.state"]],
      ["desktop_activity", t("windows.ui.desktopactivity"), ids["settings.sources.desktopActivity.state"]],
    ] as const) {
      const report = sourceByKind(dump, kind);
      sources.append(valueRow(label, selectable(automation(sourcePill(deriveSourceStatus(isSourceEnabled(dump.setup, kind), isSourcePaused(dump, kind), report), isAccessDenied(report)), id))));
    }
    if (latestBrowser) sources.append(...renderBrowserRows(latestBrowser));
    if (latestObsidian) sources.append(...renderObsidianRows(latestObsidian));
    return sources;
  }
  const sources = section(t("windows.desktop.shared.app.sourcestitle"));
  if (dump.setup?.setup_read_failed === true) {
    const failureNotice = text("div", savedChoicesUnread);
    failureNotice.classList.add("save-failure");
    sources.prepend(failureNotice);
  }
  const screen = sourceByKind(dump, "screen");
  const systemAudio = sourceByKind(dump, "system_audio");
  const mic = sourceByKind(dump, "mic");
  const desktopActivity = sourceByKind(dump, "desktop_activity");
  sources.append(
    valueRow(
      t("windows.ui.screen"),
      renderSourceControl("screen", screen, ids["settings.sources.screen.state"], ids["settings.sources.screen.enabled"], dump),
    ),
    valueRow(
      t("windows.ui.computersound"),
      renderSourceControl("system_audio", systemAudio, ids["settings.sources.systemAudio.state"], ids["settings.sources.systemAudio.enabled"], dump),
    ),
    valueRow(
      t("windows.ui.microphone"),
      renderSourceControl("mic", mic, ids["settings.sources.mic.state"], ids["settings.sources.mic.enabled"], dump),
    ),
    valueRow(
      t("windows.ui.desktopactivity"),
      renderDesktopActivityControl(desktopActivity, dump),
    ),
  );
  sources.append(renderStartAtLoginControl(dump));

  const hasSourceSaveFailure = Boolean(
    (dump.setup?.failed_sources && dump.setup.failed_sources.length > 0) ||
    dump.setup?.desktop_activity_window_titles_save_failed ||
    dump.setup?.desktop_activity_page_addresses_save_failed ||
    dump.setup?.desktop_activity_through_pause_save_failed,
  );
  if (hasSourceSaveFailure) {
    const failureEl = document.createElement("div");
    failureEl.classList.add("save-failure");
    const retryBtn = actionButton(
      t("windows.desktop.shared.settings.textcontent"),
      ids["settings.sources.saveRetry"],
      true,
      () => {
        void invoke("retry_save").then(() => retryHealth());
      },
    );
    failureEl.append(retryBtn);
    sources.append(failureEl);
  }
  if (latestBrowser) {
    sources.append(...renderBrowserRows(latestBrowser));
  }
  if (latestObsidian) {
    sources.append(...renderObsidianRows(latestObsidian));
  }
  return sources;
}

function statusSeverity(phase: AppPhase): Severity {
  switch (phase) {
    case "observing":
      return "ok";
    case "error":
      return "attention";
    case "idle":
    case "starting":
    case "paused":
      return "neutral";
  }
}

function syncSummary(sync: SyncSnapshot): string {
  const pairing = presentedPairing(sync.pairing);
  switch (pairing.phase) {
    case "not_paired":
      return t("windows.ui.pairtodelivertoyourjournal");
    case "pairing":
    case "awaiting_confirmation":
    case "failed":
      return pairingPhaseLabel(pairing);
    case "paired": {
      const upload = sync.upload;
      const quarantined = quarantinedCount(upload);
      if (quarantined > 0) {
        return needsAttentionLabel(quarantined);
      }
      if (sync.journal_link === "offline") {
        return t("windows.ui.waitingtosync");
      }
      return t("windows.ui.deliveredpending", { "value": String(upload.uploaded_segments), "value2": String(upload.pending_segments) });
    }
  }
}

function sourceGlanceToken(source: SourceReport | undefined, isMic: boolean): string {
  if (!source) {
    return t("windows.ui.notreported");
  }
  if (isAccessDenied(source)) {
    return t("windows.ui.needspermission");
  }

  switch (source.status) {
    case "active":
      return stateWord("on");
    case "inactive":
      return stateWord("off");
    case "no_input_device":
      return isMic ? t("windows.ui.nonedisplay") : t("windows.ui.noinputdevice");
    case "faulted":
      return t("windows.ui.attentionneeded");
  }
}

function sourcesGlance(dump: HealthDump): string {
  const screen = sourceGlanceToken(sourceByKind(dump, "screen"), false);
  const systemAudio = sourceGlanceToken(sourceByKind(dump, "system_audio"), false);
  const mic = sourceGlanceToken(sourceByKind(dump, "mic"), true);
  const activity = sourceGlanceToken(sourceByKind(dump, "desktop_activity"), false);
  if (screen === "on" && systemAudio === "on" && mic === "on" && activity === "on") {
    return t("windows.ui.allsourceson");
  }
  return t("windows.ui.screenarg0computersoundarg1micarg2desktopactivit", { "arg0": String(screen), "arg1": String(systemAudio), "arg2": String(mic), "arg3": String(activity) });
}

function statusLabelNode(labelText: string): HTMLElement {
  const node = text("span", labelText);
  node.classList.add("settings-status-label");
  return node;
}

function statusValueNode(value: HTMLElement): HTMLElement {
  const node = document.createElement("span");
  node.classList.add("settings-status-value");
  node.append(value);
  return node;
}

function statusTextValue(value: string): HTMLElement {
  return statusValueNode(text("span", value));
}

function statusLine(labelText: string, value: HTMLElement): HTMLElement {
  const row = document.createElement("div");
  row.classList.add("settings-status-line");
  row.append(statusLabelNode(labelText), statusValueNode(value));
  return row;
}

function statusButton(labelText: string, value: string, route: Route): HTMLButtonElement {
  const row = document.createElement("button");
  row.type = "button";
  row.classList.add("settings-status-button", "fluent-control");
  row.onclick = () => navigateTo(route);
  row.append(statusLabelNode(labelText), statusTextValue(value));
  return row;
}

// Segments end on 5-minute clock boundaries, so the countdown runs 5:00 down to
// 0:01. A rotation always sends a fresh reading; wrapping only bridges the gap.
const SEGMENT_SECS = 300;

function nextSaveRemaining(secs: number, elapsed: number): number {
  return ((((secs - elapsed - 1) % SEGMENT_SECS) + SEGMENT_SECS) % SEGMENT_SECS) + 1;
}

function nextSaveLabel(remaining: number): string {
  const secs = Math.max(0, Math.floor(remaining));
  return t("windows.time.nextsave", { minutes: String(Math.floor(secs / 60)), seconds: String(String(secs % 60).padStart(2, "0")) });
}

function lastSyncedLabel(syncedAtMs: number | null | undefined, secsNow: number): string {
  if (syncedAtMs == null) {
    return t("windows.ui.notyet");
  }
  const secs = Math.max(0, secsNow - Math.floor(syncedAtMs / 1000));
  if (secs < 60) {
    return t("windows.desktop.shared.app.justnow");
  }
  if (secs < 3600) {
    const m = Math.floor(secs / 60);
    return t("windows.ui.minuteago", { count: m });
  }
  if (secs < 86400) {
    const h = Math.floor(secs / 3600);
    return t("windows.ui.hourago", { count: h });
  }
  const d = Math.floor(secs / 86400);
  return t("windows.ui.dayago", { count: d });
}

function updateStatusClocks(): void {
  const now = nowSecs();
  const elapsed = healthReceivedAt == null ? 0 : now - healthReceivedAt;
  const secs = latestHealth?.segment_seconds_remaining;
  if (nextSaveEl && secs != null) {
    nextSaveEl.textContent = nextSaveLabel(nextSaveRemaining(secs, elapsed));
  }
  if (lastSyncedEl && latestHealth) {
    lastSyncedEl.textContent = lastSyncedLabel(latestHealth.sync.upload.last_successful_sync, now);
  }
}

function renderStatusStrip(dump: HealthDump): HTMLElement {
  const strip = document.createElement("div");
  strip.classList.add("settings-status-strip");
  const statePill = pill(statusStateLabel(dump), statusSeverity(dump.app_state));
  trackPausePill(dump, statePill);
  strip.append(
    statusLine(
      t("windows.ui.state"),
      automation(statePill, ids["settings.status.appState.state"]),
    ),
  );
  if (dump.app_state === "observing" && dump.segment_seconds_remaining != null) {
    nextSaveEl = automation(text("span", nextSaveLabel(dump.segment_seconds_remaining)), ids["settings.status.nextSave"]);
    strip.append(statusLine(t("windows.ui.nextsave"), nextSaveEl));
  }
  strip.append(statusButton(t("windows.desktop.shared.about.pendingsettingstab"), syncSummary(dump.sync), "journal"));
  if (dump.sync.pairing.phase === "paired") {
    lastSyncedEl = automation(
      text("span", lastSyncedLabel(dump.sync.upload.last_successful_sync, nowSecs())),
      ids["settings.status.lastSynced"],
    );
    strip.append(statusLine(t("windows.ui.journallastreached"), lastSyncedEl));
  }
  strip.append(statusButton(t("windows.desktop.shared.app.sourcestitle"), sourcesGlance(dump), "sources"));
  return strip;
}

function homeCard(titleText: string, glanceNode: HTMLElement, actionNode: HTMLElement): HTMLElement {
  const card = document.createElement("div");
  card.classList.add("settings-card");

  const title = text("h2", titleText);
  title.classList.add("settings-card-title");

  const glance = document.createElement("div");
  glance.classList.add("settings-card-glance");
  glance.append(glanceNode);

  const action = document.createElement("div");
  action.classList.add("settings-card-action");
  action.append(actionNode);

  card.append(title, glance, action);
  return card;
}

function storageGlance(): HTMLElement {
  return text("div", t("windows.ui.storedonthispc"));
}

function globalPauseActive(dump: HealthDump): boolean {
  return dump.pause !== null || dump.app_state === "paused";
}

function reconcileWizardPauseFailure(dump: HealthDump): void {
  const active = globalPauseActive(dump);
  if (wizardPauseFailure === "pause" && active) wizardPauseFailure = null;
  if (wizardPauseFailure === "resume" && !active) wizardPauseFailure = null;
}

function reconcileUnknownLoginAttempt(dump: HealthDump): void {
  const choice = dump.setup?.start_at_login;
  if ((choice === true || choice === false) && !dump.setup?.login_apply_failed) {
    unknownLoginAttempt = null;
  }
}

// Home keeps its phase gate: pause only while observing, resume while paused,
// restart from error, and a disabled pause otherwise. Setup uses the control below.
function createPauseButton(dump: HealthDump): HTMLButtonElement {
  const phase = latestHealth?.app_state ?? dump.app_state;
  // The Obsidian source runs beside capture and leaves the phase idle.
  const obsidianOn = latestObsidian !== null && latestObsidian.state !== "off";
  // So does screenshots, once it is taking them in.
  const screenshotsOn =
    latestScreenshots !== null &&
    (latestScreenshots.state === "on" || latestScreenshots.state === "bringing_in");
  if (phase === "observing" || (phase === "idle" && (obsidianOn || screenshotsOn))) {
    return actionButton(t("windows.desktop.shared.setup.wizardpausefailure"), undefined, true, async () => {
      try {
        await invoke("pause", { reason: "operator", durationSecs: null });
        retryHealth();
      } catch {
        // The home card does not keep a rejection. Setup stores its own.
      }
    });
  } else if (phase === "paused") {
    return actionButton(t("windows.desktop.shared.setup.wizardpausefailure2"), undefined, true, async () => {
      try {
        await invoke("resume");
        retryHealth();
      } catch {
        // The home card does not keep a rejection. Setup stores its own.
      }
    });
  } else if (phase === "error") {
    return actionButton(t("windows.ui.restartintake"), undefined, true, async () => {
      try {
        await invoke("resume");
        retryHealth();
      } catch {
        // The home card does not keep a rejection. Setup stores its own.
      }
    });
  } else {
    return actionButton(t("windows.desktop.shared.setup.wizardpausefailure"), undefined, false, () => {});
  }
}

function createSetupPauseButton(dump: HealthDump): HTMLButtonElement {
  if (globalPauseActive(dump)) {
    return actionButton(t("windows.desktop.shared.setup.wizardpausefailure2"), undefined, true, async () => {
      try {
        await invoke("resume");
        wizardPauseFailure = null;
      } catch {
        wizardPauseFailure = "resume";
      } finally {
        void retryHealth();
      }
    });
  }
  return actionButton(t("windows.desktop.shared.setup.wizardpausefailure"), undefined, true, async () => {
    try {
      await invoke("pause", { reason: "operator", durationSecs: null });
      wizardPauseFailure = null;
    } catch {
      wizardPauseFailure = "pause";
    } finally {
      void retryHealth();
    }
  });
}

function renderPauseCard(dump: HealthDump): HTMLElement {
  const action = createPauseButton(dump);
  const statePill = pill(statusStateLabel(dump), statusSeverity(dump.app_state));
  trackPausePill(dump, statePill);

  return homeCard(t("windows.ui.pauseresume"), statePill, action);
}

function renderJournalCard(dump: HealthDump): HTMLElement {
  const phase = dump.sync.pairing.phase;
  const labelText = phase === "not_paired" || phase === "failed" ? t("windows.desktop.shared.setup.pair") : t("windows.ui.journaldetails");
  return homeCard(
    t("windows.desktop.shared.about.pendingsettingstab"),
    text("div", pairingPhaseLabel(dump.sync.pairing)),
    actionButton(labelText, undefined, true, () => navigateTo("journal")),
  );
}

function renderStorageCard(): HTMLElement {
  return homeCard(
    t("windows.ui.storage"),
    storageGlance(),
    actionButton(t("windows.ui.openfolder"), undefined, true, () => void invoke("open_storage_folder")),
  );
}

function renderUpdatesCard(): HTMLElement {
  return homeCard(
    t("windows.desktop.shared.settings.updates"),
    text("div", latestUpdate ? updateHeadline(latestUpdate) : t("windows.ui.notavailablerightnow")),
    actionButton(t("windows.desktop.shared.updates.checknow"), undefined, latestUpdate !== null, () => void invoke("update_check_now")),
  );
}

function renderSourcesNotice(dump: HealthDump = latestHealth ?? ({} as HealthDump)): HTMLElement {
  const card = document.createElement("div");
  card.classList.add("settings-card");
  automation(card, ids["settings.home.sourcesNotice"]);

  const heading = text("h2", t("windows.ui.youchoosewhichsourcesareon"));
  heading.classList.add("settings-card-title");

  const body = text(
    "div",
    t("windows.ui.youcanturneachsourceonor"),
  );
  body.style.fontSize = "13px";
  body.style.color = "var(--fg-subtle)";
  body.style.margin = "4px 0 12px";

  const dismiss = actionButton(
    t("windows.desktop.shared.updates.dismiss"),
    ids["settings.home.sourcesNotice.dismiss"],
    true,
    () => {
      void invoke("dismiss_notice").then(() => retryHealth());
    },
  );

  card.append(heading, body, dismiss);

  if (dump.setup?.notice_save_failed) {
    const failEl = text("div", t("windows.ui.couldnotsavethisdismissaltryagain"));
    failEl.classList.add("save-failure");
    const retryBtn = actionButton(
      t("windows.desktop.shared.settings.textcontent"),
      ids["settings.sources.saveRetry"],
      true,
      () => {
        void invoke("retry_save").then(() => retryHealth());
      },
    );
    card.append(failEl, retryBtn);
  }

  return card;
}

function setAcceptedProofGeneration(generation: number): void {
  currentAcceptedProofGeneration = generation;
}

function clearVisibleProof(): void {
  latestPreview = {};
  for (const [kind, id] of [
    ["mic", ids["settings.setup.mic.proof"]],
    ["system_audio", ids["settings.setup.systemAudio.proof"]],
    ["screen", ids["settings.setup.screen.proof"]],
  ] as const) {
    const node = document.querySelector<HTMLElement>(`[data-automation-id="${id}"]`);
    if (node) fillProofNode(node, kind, {});
  }
}

function closeProofConsumer(): void {
  ++proofRequest;
  currentAcceptedProofGeneration = 0;
  openProofKind = "closed";
  clearVisibleProof();
  proofOperations = proofOperations.catch(() => {}).then(() => invoke("set_proof_consumer", { source: null })).catch(() => {});
}

function proofEligible(kind: SourceKind, dump: HealthDump | null = latestHealth): boolean {
  return Boolean(dump && deriveSourceStatus(
    isSourceEnabled(dump.setup, kind), dump.pause !== null || dump.app_state === "paused",
    sourceByKind(dump, kind),
  ) === "on");
}

function ensureProofConsumer(step: WizardStep): void {
  const target: ProofConsumerState = proofWindowActive && document.visibilityState !== "hidden" &&
    (step === "mic" || step === "system_audio" || step === "screen") && proofEligible(step) ? step : "closed";
  if (openProofKind === target) return;
  if (target === "closed") {
    closeProofConsumer();
    return;
  }
  const request = ++proofRequest;
  currentAcceptedProofGeneration = 0;
  openProofKind = target;
  clearVisibleProof();
  proofOperations = proofOperations.catch(() => {}).then(() => invoke<number>("set_proof_consumer", { source: target })).then((gen) => {
    if (request === proofRequest && openProofKind === target && typeof gen === "number") {
      currentAcceptedProofGeneration = gen;
      clearVisibleProof();
    }
  }).catch(() => {
    if (request === proofRequest) {
      currentAcceptedProofGeneration = 0;
      openProofKind = "closed";
      clearVisibleProof();
    }
  });
}

function setProofWindowActive(active: boolean): void {
  proofWindowActive = active;
  if (!active) closeProofConsumer();
  else if (label === "settings" && activeRoute === "home" && latestHealth?.setup) {
    ensureProofConsumer(latestHealth.setup.wizard);
  }
}

function handleProofInactive(): void {
  setProofWindowActive(false);
  // A native notice may arrive after focus returned and a newer open completed.
  // Clear that proof, then ask native authority afresh instead of latching stale inactivity.
  if (document.hasFocus() && document.visibilityState !== "hidden") {
    setProofWindowActive(true);
  }
}

function fillProofNode(node: HTMLElement, kind: SourceKind, preview: PreviewSnapshot): void {
  const enabled = latestHealth ? isSourceEnabled(latestHealth.setup, kind) : false;
  const isPaused = latestHealth ? (latestHealth.pause !== null || latestHealth.app_state === "paused") : false;
  const report = latestHealth ? sourceByKind(latestHealth, kind) : undefined;
  const status = deriveSourceStatus(enabled, isPaused, report);

  if (!enabled) {
    // The control's own state pill already says off; nothing to prove yet.
    node.replaceChildren();
    return;
  }

  if (status === "off" || isPaused || !proofWindowActive || document.visibilityState === "hidden") {
    const withheldEl = text("span", isPaused ? t("windows.ui.pausedword") : t("windows.ui.previewpaused"));
    withheldEl.classList.add("proof-status");
    node.replaceChildren(withheldEl);
    return;
  }

  if (status === "unavailable" || status === "error") {
    const reason = report?.status === "no_input_device"
      ? (kind === "mic" ? NO_MICROPHONE_LINE : NO_OUTPUT_LINE)
      : report?.reason === "access_denied" || report?.reason === "AccessDenied"
        ? ACCESS_DENIED_LINE
        : undefined;
    const noDevice = reason === NO_MICROPHONE_LINE || reason === NO_OUTPUT_LINE;
    const textEl = text("span", reason ?? status);
    textEl.classList.add("proof-status");
    // A missing device is picked up on its own, so there is nothing to retry.
    if (noDevice) {
      node.replaceChildren(textEl);
      return;
    }
    const retryBtn = actionButton(t("windows.desktop.shared.settings.textcontent"), undefined, true, () => {
      void invoke("set_source_enabled", { kind, enabled: true }).then(() => retryHealth());
    }, false, "standard");
    retryBtn.id = "setup-proof-retry";
    retryBtn.style.marginLeft = "12px";
    node.replaceChildren(textEl, retryBtn);
    return;
  }

  const validSnapshot =
    typeof preview.consumer_generation === "number" &&
    preview.consumer_generation !== 0 &&
    preview.consumer_generation === currentAcceptedProofGeneration
      ? preview
      : {};

  if (kind === "mic" || kind === "system_audio") {
    const level = kind === "mic" ? validSnapshot.mic_level : validSnapshot.system_audio_level;
    if (typeof level === "number" && Number.isFinite(level)) {
      const meter = document.createElement("meter");
      meter.setAttribute("role", "meter");
      meter.setAttribute("aria-label", kind === "mic" ? t("windows.ui.microphonelevel") : t("windows.ui.computersoundlevel"));
      meter.min = 0;
      meter.max = 1;
      meter.value = level;
      meter.setAttribute("aria-valuemin", "0");
      meter.setAttribute("aria-valuemax", "1");
      meter.setAttribute("aria-valuenow", String(level));
      node.replaceChildren(meter);
    } else {
      const waitingEl = text("span", t("windows.ui.waiting"));
      waitingEl.classList.add("proof-status");
      node.replaceChildren(waitingEl);
    }
  } else if (kind === "screen") {
    const frame = validSnapshot.screen_frame;
    if (frame && Array.isArray(frame.rgba) && typeof frame.width === "number" && typeof frame.height === "number") {
      const canvas = document.createElement("canvas");
      canvas.setAttribute("aria-label", t("windows.ui.screenpreview"));
      canvas.width = frame.width;
      canvas.height = frame.height;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        try {
          const imgData = ctx.createImageData(frame.width, frame.height);
          imgData.data.set(new Uint8ClampedArray(frame.rgba));
          ctx.putImageData(imgData, 0, 0);
        } catch {
          // ignore canvas paint errors
        }
      }
      const caption = text("p", t("windows.desktop.shared.copy.setupscreenframecaption"));
      caption.classList.add("setup-screen-caption");
      node.replaceChildren(canvas, caption);
    } else {
      const waitingEl = text("span", t("windows.ui.waiting"));
      waitingEl.classList.add("proof-status");
      node.replaceChildren(waitingEl);
    }
  }
}

function renderProofNode(kind: SourceKind, preview: PreviewSnapshot): HTMLElement {
  const node = document.createElement("div");
  node.classList.add("wizard-proof", "proof-node");
  if (kind === "mic") {
    automation(node, ids["settings.setup.mic.proof"]);
  } else if (kind === "system_audio") {
    automation(node, ids["settings.setup.systemAudio.proof"]);
  } else if (kind === "screen") {
    automation(node, ids["settings.setup.screen.proof"]);
  }
  fillProofNode(node, kind, preview);
  return node;
}

function applyPreview(preview: PreviewSnapshot): void {
  if (
    !proofWindowActive || document.visibilityState === "hidden" ||
    typeof preview.consumer_generation !== "number" ||
    preview.consumer_generation === 0 ||
    preview.consumer_generation !== currentAcceptedProofGeneration
  ) {
    return;
  }
  const proofNode = document.querySelector<HTMLElement>(".wizard-proof");
  if (!proofNode) {
    return;
  }
  const autoId = proofNode.dataset.automationId;
  let kind: SourceKind | null = null;
  if (autoId === ids["settings.setup.mic.proof"]) {
    kind = "mic";
  } else if (autoId === ids["settings.setup.systemAudio.proof"]) {
    kind = "system_audio";
  } else if (autoId === ids["settings.setup.screen.proof"]) {
    kind = "screen";
  }
  if (!kind || !proofEligible(kind)) {
    return;
  }
  latestPreview = preview;
  fillProofNode(proofNode, kind, preview);
}

function renderWizardSourceControl(
  kind: SourceKind,
  automationId: string,
  dump: HealthDump,
): HTMLElement {
  const enabled = isSourceEnabled(dump.setup, kind);
  const isPaused = dump.pause !== null || dump.app_state === "paused";
  const report = sourceByKind(dump, kind);
  const status = deriveSourceStatus(enabled, isPaused, report);
  const failure = dump.setup?.failed_sources?.find((f) => f.kind === kind);

  const accessibleName =
    kind === "mic" ? t("windows.ui.microphone") : kind === "system_audio" ? t("windows.ui.computersound") : t("windows.ui.screen");

  const toggle = toggleRow(enabled ? accessibleName : t("windows.ui.turnon", { "accessiblename": String(accessibleName) }), automationId, enabled, async (on) => {
    try {
      await invoke("set_source_enabled", { kind, enabled: on });
    } finally {
      retryHealth();
    }
  });
  toggle.style.fontSize = "16px";
  toggle.style.padding = "12px 0";
  const input = toggle.querySelector("input");
  if (input) {
    input.setAttribute("aria-label", enabled ? accessibleName : t("windows.ui.turnon", { "accessiblename": String(accessibleName) }));
    input.style.width = "20px";
    input.style.height = "20px";
    if (status !== null) {
      input.dataset.status = status;
    }
  }
  if (status !== null) {
    toggle.dataset.status = status;
    toggle.append(sourcePill(status, isAccessDenied(report)));
  }
  const control = document.createElement("div");
  control.append(toggle);
  if (failure) {
    const failEl = text("div", sourceSaveFailureMessage(failure));
    failEl.classList.add("save-failure");
    const retry = actionButton(t("windows.desktop.shared.settings.textcontent"), ids["settings.sources.saveRetry"], true, () => {
      void invoke("retry_save").then(() => retryHealth());
    });
    retry.style.marginTop = "8px";
    failEl.append(document.createElement("br"), retry);
    control.append(failEl);
  }
  return control;
}

async function commitUnknownLogin(enabled: boolean): Promise<void> {
  try {
    await invoke("set_start_at_login", { enabled });
    unknownLoginAttempt = null;
  } catch {
    unknownLoginAttempt = enabled;
  } finally {
    void retryHealth();
  }
}

function renderStartAtLoginControl(dump: HealthDump): HTMLElement {
  reconcileUnknownLoginAttempt(dump);
  const wrap = document.createElement("div");
  wrap.classList.add("wizard-start-at-login");

  if (dump.setup?.start_at_login === null) {
    const toggle = toggleRow(
      t("windows.desktop.shared.setup.startatlogin"),
      ids["settings.setup.startAtLogin"],
      false,
      () => {},
    );
    const box = toggle.querySelector("input");
    if (box) {
      box.indeterminate = true;
      box.disabled = true;
    }
    const onBtn = actionButton(t("windows.desktop.shared.setup.startatlogin"), undefined, true, () => {
      void commitUnknownLogin(true);
    });
    onBtn.id = "setup-login-on";
    const offBtn = actionButton(t("windows.ui.dontstartatlogin"), undefined, true, () => {
      void commitUnknownLogin(false);
    });
    offBtn.id = "setup-login-off";
    wrap.style.display = "flex";
    wrap.style.alignItems = "center";
    wrap.style.gap = "8px";
    wrap.append(toggle, onBtn, offBtn);
    if (unknownLoginAttempt !== null || dump.setup.login_apply_failed) {
      const err = text("div", t("windows.ui.couldnotupdatestartatlogin"));
      automation(err, ids["settings.setup.startAtLogin.error"]);
      err.classList.add("save-failure");
      const retry = actionButton(t("windows.desktop.shared.settings.textcontent"), undefined, true, () => {
        if (unknownLoginAttempt !== null) {
          void commitUnknownLogin(unknownLoginAttempt);
          return;
        }
        void invoke("retry_save").then(() => retryHealth());
      });
      retry.id = "setup-login-retry";
      wrap.append(err, retry);
    }
    return wrap;
  }

  const checked = dump.setup?.start_at_login === true;
  const toggle = toggleRow(
    t("windows.desktop.shared.setup.startatlogin"),
    ids["settings.setup.startAtLogin"],
    checked,
    async (on) => {
      try {
        await invoke("set_start_at_login", { enabled: on });
      } finally {
        retryHealth();
      }
    },
  );
  wrap.append(toggle);
  if (dump.setup?.login_apply_failed) {
    const err = text("div", t("windows.ui.couldnotupdatestartatlogin"));
    automation(err, ids["settings.setup.startAtLogin.error"]);
    err.classList.add("wizard-error");
    const retry = actionButton(t("windows.desktop.shared.settings.textcontent"), undefined, true, () => {
      void invoke("retry_save").then(() => retryHealth());
    });
    retry.id = "setup-login-retry";
    wrap.append(err, retry);
  }
  return wrap;
}

async function advanceWizardStep(step: WizardStep): Promise<void> {
  try {
    const res = await invoke<{ saved: boolean; proof_generation: number }>("set_wizard_step", {
      step,
    });
    if (res && res.saved === true) {
      closeProofConsumer();
      focusWizardPrimary = true;
    }
  } finally {
    retryHealth();
  }
}

function renderSetupProgress(stepNum: number, totalSteps: number): HTMLElement {
  const progressText = t("windows.ui.progresstext", { "stepnum": Number(stepNum), "totalsteps": Number(totalSteps) });
  const p = document.createElement("p");
  p.classList.add("setup-progress", "wizard-progress");
  p.dataset.stepNumber = String(stepNum);
  automation(p, ids["settings.setup.stepNumber"]);
  p.setAttribute("aria-label", progressText);

  const textSpan = text("span", progressText);
  textSpan.classList.add("setup-progress-text");
  textSpan.setAttribute("aria-hidden", "true");

  const dotsSpan = document.createElement("span");
  dotsSpan.classList.add("setup-progress-dots");
  dotsSpan.setAttribute("aria-hidden", "true");

  for (let i = 1; i <= totalSteps; i++) {
    const dot = document.createElement("span");
    dot.classList.add("setup-dot");
    dot.setAttribute("aria-hidden", "true");
    const state = i < stepNum ? "done" : i === stepNum ? "current" : "future";
    dot.dataset.state = state;
    if (totalSteps === 8 && i === 5) {
      dot.dataset.gap = "group";
    }
    dotsSpan.append(dot);
  }

  p.append(dotsSpan, textSpan);
  return p;
}

function renderSetupBand(): HTMLElement {
  const header = document.createElement("header");
  header.classList.add("setup-band");

  try {
    const doc = new DOMParser().parseFromString(lockupSvg, "image/svg+xml");
    const docEl = doc.documentElement;
    if (docEl && docEl.tagName.toLowerCase() === "svg") {
      const imported = document.importNode(docEl, true);
      header.append(imported);
    }
  } catch {
    // ignore parsing errors
  }

  const labelNode = text("span", t("windows.desktop.shared.copy.setupapplabel"));
  labelNode.classList.add("setup-band-label");

  header.append(labelNode);
  return header;
}

function renderSetupFrame(dump: HealthDump): HTMLElement {
  const frame = document.createElement("div");
  frame.classList.add("setup-frame", "settings-wizard");
  frame.append(renderSetupBand());

  const sheet = document.createElement("div");
  sheet.classList.add("setup-sheet");
  sheet.append(renderWizard(dump, latestPreview));

  frame.append(sheet);
  return frame;
}

// What the screen step turns on, where it is kept, and what it leaves out. The password
// manager defaults match each app's own process, so the line names apps.
const SETUP_SCREEN_TRUST_LINES = [
  t("windows.ui.screenalsoaddstheappsyouuse"),
  t("windows.ui.whatyousharestaysonthispc"),
  t("windows.ui.n1passwordbitwardenandtheotherpasswordmanager"),
] as const;

function renderWizard(dump: HealthDump, preview: PreviewSnapshot): HTMLElement {
  reconcileWizardPauseFailure(dump);

  const step = dump.setup?.wizard ?? "mic";
  ensureProofConsumer(step);

  const stepContainer = document.createElement("div");
  stepContainer.classList.add("setup-step");
  automation(stepContainer, ids["settings.setup.step"]);
  stepContainer.dataset.step = step;

  const continuingFromJournal = Boolean(dump.setup?.handoff_id);
  const totalSteps = continuingFromJournal ? 8 : 4;
  const stepNumberMap = continuingFromJournal
    ? { mic: 5, system_audio: 6, screen: 7, connection: 8 }
    : { mic: 1, system_audio: 2, screen: 3, connection: 4 };

  const headings = {
    mic: t("windows.desktop.shared.copy.setupmicrophoneheadline"),
    system_audio: t("windows.ui.sharesoundfromthiscomputer"),
    screen: t("windows.ui.shareyourscreen"),
    connection: t("windows.ui.connectyourjournal"),
  } as const;

  const stepNum = stepNumberMap[step as keyof typeof stepNumberMap] ?? 1;

  const columns = document.createElement("div");
  columns.classList.add("setup-columns");

  const copy = document.createElement("div");
  copy.classList.add("setup-copy");

  const heading = text("h2", headings[step as keyof typeof headings] ?? "");
  heading.classList.add("wizard-step-title");
  heading.tabIndex = -1;

  if (step === "connection") {
    const isPaired = dump.sync.pairing.phase === "paired";
    if (isPaired) {
      if (dump.sync.pairing.mark != null) {
        heading.textContent = t("windows.ui.connectedto", { "value": String(markTextForm(dump.sync.pairing.mark)) });
      } else {
        heading.textContent = t("windows.desktop.shared.copy.setupconnectedtoyourjournal");
      }
      automation(heading, ids["settings.pairing.state"]);
      heading.classList.add("selectable");
    }
  }

  copy.append(heading);

  if (step !== "connection") {
    const sentence = text("p", t("windows.ui.whatyouturnongoesintoyour"));
    sentence.classList.add("wizard-sentence");
    copy.append(sentence);
  }

  if (step === "screen") {
    for (const line of SETUP_SCREEN_TRUST_LINES) {
      const sentence = text("p", line);
      sentence.classList.add("wizard-sentence");
      copy.append(sentence);
    }
  }

  const card = document.createElement("div");
  card.classList.add("setup-card");

  const footer = document.createElement("div");
  footer.classList.add("setup-footer");

  if (step === "mic" || step === "system_audio" || step === "screen") {
    const kind: SourceKind = step;
    const autoId =
      kind === "mic"
        ? "settings.setup.mic"
        : kind === "system_audio"
          ? "settings.setup.systemAudio"
          : "settings.setup.screen";
    const nextStep: WizardStep =
      kind === "mic" ? "system_audio" : kind === "system_audio" ? "screen" : "connection";
    const prevStep: WizardStep =
      kind === "system_audio" ? "mic" : "system_audio";

    const sw = renderWizardSourceControl(kind, autoId, dump);
    const switchRow = document.createElement("div");
    switchRow.classList.add("setup-switch-row");
    const labelEl = sw.querySelector("label");
    if (labelEl) {
      switchRow.append(labelEl);
    }
    if (isSourceEnabled(dump.setup, kind)) {
      const aside = document.createElement("div");
      aside.classList.add("setup-switch-aside");
      const pauseBtn = createSetupPauseButton(dump);
      pauseBtn.id = "setup-pause";
      const hint = text("span", t("windows.ui.pausesallsources"));
      hint.classList.add("setup-pause-hint");
      aside.append(pauseBtn, hint);
      switchRow.append(aside);
    }
    card.append(switchRow);

    while (sw.firstChild) {
      card.append(sw.firstChild);
    }

    const proof = renderProofNode(kind, preview);
    card.append(proof);

    if (step === "mic") {
      card.append(renderStartAtLoginControl(dump));
    }

    if (isSourceEnabled(dump.setup, kind) && wizardPauseFailure) {
      const pauseFailureEl = text(
        "div",
        wizardPauseFailure === "pause" ? t("windows.ui.couldnotpausetryagain") : t("windows.ui.couldnotresumetryagain"),
      );
      pauseFailureEl.classList.add("save-failure");
      card.append(pauseFailureEl);
    }

    if (dump.setup?.wizard_save_failed) {
      const failEl = text("div", t("windows.ui.couldnotsaveyoursetupprogresstry"));
      failEl.classList.add("save-failure");
      card.append(failEl);
    }

    const anyWizardSaveFailed = Boolean(
      (dump.setup?.failed_sources && dump.setup.failed_sources.length > 0) ||
      dump.setup?.wizard_save_failed ||
      dump.setup?.notice_save_failed
    );
    const currentSourceFailure = dump.setup?.failed_sources?.some((failure) => failure.kind === step);
    if (anyWizardSaveFailed && !currentSourceFailure) {
      const retryFailEl = document.createElement("div");
      retryFailEl.classList.add("save-failure");
      const retryBtn = actionButton(
        t("windows.desktop.shared.settings.textcontent"),
        ids["settings.sources.saveRetry"],
        true,
        () => {
          void invoke("retry_save").then(() => retryHealth());
        },
      );
      retryFailEl.append(retryBtn);
      card.append(retryFailEl);
    }

    const notNowBtn = actionButton(
      t("windows.desktop.shared.copy.deferchoice"),
      ids["settings.setup.notNow"],
      true,
      async () => {
        try {
          await invoke("set_source_enabled", { kind, enabled: false });
          await advanceWizardStep(nextStep);
        } finally {
          retryHealth();
        }
      },
      false,
      "standard",
    );
    notNowBtn.classList.add("setup-text");
    card.append(notNowBtn);

    if (step !== "mic") {
      const backBtn = actionButton(
        t("windows.desktop.shared.copy.setupback"),
        ids["settings.setup.back"],
        true,
        async () => {
          await advanceWizardStep(prevStep);
        },
        false,
        "standard",
      );
      backBtn.classList.add("setup-back");
      backBtn.setAttribute("aria-label", t("windows.desktop.shared.copy.setupback"));
      footer.append(backBtn);
    }

    footer.append(renderSetupProgress(stepNum, totalSteps));

    const spacer = document.createElement("span");
    spacer.classList.add("setup-footer-spacer");
    const downloads = document.createElement("span");
    downloads.classList.add("setup-downloads");
    downloads.setAttribute("aria-hidden", "true");
    footer.append(spacer, downloads);

    if (isSourceEnabled(dump.setup, kind)) {
      const continueBtn = actionButton(
        t("windows.desktop.shared.copy.setupcontinue"),
        ids["settings.setup.next"],
        true,
        async () => {
          await advanceWizardStep(nextStep);
        },
        false,
        "accent",
      );
      continueBtn.classList.add("setup-primary");
      footer.append(continueBtn);
    }
  } else if (step === "connection") {
    card.append(renderPairingSection(dump, { setupStep: true }));

    if (dump.setup?.wizard_save_failed) {
      const failEl = text("div", t("windows.ui.couldnotsaveyoursetupprogresstry"));
      failEl.classList.add("save-failure");
      card.append(failEl);
    }

    const anyWizardSaveFailed = Boolean(
      (dump.setup?.failed_sources && dump.setup.failed_sources.length > 0) ||
      dump.setup?.wizard_save_failed ||
      dump.setup?.notice_save_failed
    );
    if (anyWizardSaveFailed) {
      const retryFailEl = document.createElement("div");
      retryFailEl.classList.add("save-failure");
      const retryBtn = actionButton(
        t("windows.desktop.shared.settings.textcontent"),
        ids["settings.sources.saveRetry"],
        true,
        () => {
          void invoke("retry_save").then(() => retryHealth());
        },
      );
      retryFailEl.append(retryBtn);
      card.append(retryFailEl);
    }

    const backBtn = actionButton(
      t("windows.desktop.shared.copy.setupback"),
      ids["settings.setup.back"],
      true,
      async () => {
        await advanceWizardStep("screen");
      },
      false,
      "standard",
    );
    backBtn.classList.add("setup-back");
    backBtn.setAttribute("aria-label", t("windows.desktop.shared.copy.setupback"));
    footer.append(backBtn);

    footer.append(renderSetupProgress(stepNum, totalSteps));

    const spacer = document.createElement("span");
    spacer.classList.add("setup-footer-spacer");
    const downloads = document.createElement("span");
    downloads.classList.add("setup-downloads");
    downloads.setAttribute("aria-hidden", "true");
    footer.append(spacer, downloads);

    const isPaired = dump.sync.pairing.phase === "paired";
    const forwardBtn = isPaired
      ? actionButton(
          t("windows.ui.next"),
          ids["settings.setup.next"],
          true,
          async () => {
            if (!nativeContinuationPending(dump)) {
              await advanceWizardStep("done");
            }
          },
          false,
          "accent",
        )
      : actionButton(
          t("windows.ui.skip"),
          ids["settings.setup.notNow"],
          true,
          async () => {
            if (!nativeContinuationPending(dump)) {
              await advanceWizardStep("done");
            }
          },
          false,
          "standard",
        );

    if (isPaired) {
      forwardBtn.classList.add("setup-primary");
    } else {
      forwardBtn.classList.add("setup-text");
    }

    if (!nativeContinuationPending(dump)) {
      footer.append(forwardBtn);
    }
  }

  columns.append(copy, card);
  stepContainer.append(columns, footer);
  return stepContainer;
}

function renderHome(dump: HealthDump): HTMLElement {
  const home = document.createElement("div");
  home.classList.add("settings-home");

  const cards = document.createElement("div");
  cards.classList.add("settings-card-grid");
  cards.append(
    renderPauseCard(dump),
    renderJournalCard(dump),
    renderStorageCard(),
    renderUpdatesCard(),
  );

  if (dump.setup?.sequence_enabled === true && dump.setup.wizard === "done" && !dump.setup.notice_dismissed) {
    home.append(renderSourcesNotice(), renderStatusStrip(dump), cards);
  } else {
    home.append(renderStatusStrip(dump), cards);
  }
  if (dump.setup?.setup_read_failed === true) {
    const failureNotice = text("div", savedChoicesUnread);
    failureNotice.classList.add("save-failure");
    home.prepend(failureNotice);
    home.append(renderStartAtLoginControl(dump));
  }
  return home;
}

function renderRouteContent(route: Route, dump: HealthDump): HTMLElement {
  const content = document.createElement("div");
  content.classList.add("settings-route-content");

  switch (route) {
    case "home":
      if (settingsShowsSetupFrame(dump)) {
        // The shell must not also mount a wizard.
      } else {
        const home = renderHome(dump);
        if (homeShowsPairing(dump)) {
          home.append(renderPairingSection(dump));
        }
        content.append(home);
      }
      break;
    case "sources":
      content.append(renderSourcesSection(dump), latestMic ? renderMicSection(latestMic) : loadingCaption());
      break;
    case "privacy":
      content.append(latestExclusions ? renderExclusionsSection(latestExclusions, dump) : loadingCaption());
      break;
    case "journal": {
      const sync = section(t("windows.ui.sync"));
      sync.append(syncRow(dump.sync));
      const sections = [
        renderJournalOpenSection(dump),
        renderPairingSection(dump),
        sync,
      ];
      if (dump.sync.unknown_journals && dump.sync.unknown_journals.length > 0) {
        const unknownSection = renderUnknownJournalsSection(dump.sync.unknown_journals);
        if (unknownSection) {
          sections.push(unknownSection);
        }
      }
      content.append(...sections);
      break;
    }
    case "shortcut":
      content.append(latestHotkey ? renderHotkeySection(latestHotkey) : loadingCaption());
      break;
    case "storage": {
      const location = section(t("windows.ui.storagelocation"));
      location.append(
        storageRow(latestStorage),
        helpCaption(t("windows.ui.segmentsthathavenotreachedyourjournal")),
        helpCaption(
          t("windows.ui.asegmentisa5minutelocal"),
        ),
      );
      content.append(location);
      break;
    }
    case "updates":
      content.append(latestUpdate ? renderUpdatesSection(latestUpdate) : loadingCaption());
      break;
    case "help":
      logHelp ??= createLogHelp();
      content.append(logHelp);
      break;
  }

  clearLastSectionDivider(content);
  return content;
}

function navButton(route: Route, labelText: string, glyph: string): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.classList.add("settings-nav-item", "fluent-control");
  if (route === activeRoute) {
    button.setAttribute("aria-current", "page");
  }

  const icon = text("span", glyph);
  icon.classList.add("settings-nav-glyph");
  icon.setAttribute("aria-hidden", "true");

  const labelNode = text("span", labelText);
  button.append(icon, labelNode);
  return button;
}

function renderRail(): HTMLElement {
  const rail = document.createElement("nav");
  rail.id = "settings-nav";
  rail.classList.add("settings-rail");
  rail.setAttribute("aria-label", t("windows.desktop.shared.app.frontendorigin"));

  const title = text("div", "solstone");
  title.classList.add("settings-rail-title");
  rail.append(title);

  for (const item of ROUTES) {
    const button = navButton(item.route, item.label, item.glyph);
    button.onclick = () => navigateTo(item.route);
    rail.append(button);
  }

  return rail;
}

function renderPane(dump: HealthDump): HTMLElement {
  const pane = document.createElement("main");
  pane.classList.add("settings-pane");

  const frame = document.createElement("div");
  frame.classList.add("settings-pane-frame");

  const topbar = document.createElement("div");
  topbar.classList.add("settings-pane-topbar");

  const hamburger = document.createElement("button");
  hamburger.type = "button";
  hamburger.classList.add("settings-hamburger", "fluent-control");
  hamburger.setAttribute("aria-label", t("windows.ui.opennavigation"));
  hamburger.setAttribute("aria-expanded", "false");
  hamburger.setAttribute("aria-controls", "settings-nav");
  hamburger.onclick = () => {
    const shell = settingsShell();
    if (shell && navOverlayOpen(shell)) {
      closeNavOverlay();
    } else {
      openNavOverlay();
    }
  };

  const hamburgerGlyph = text("span", "\uE700");
  hamburgerGlyph.classList.add("settings-hamburger-glyph");
  hamburgerGlyph.setAttribute("aria-hidden", "true");
  hamburger.append(hamburgerGlyph);

  const inSetup = settingsShowsSetupFrame(dump);
  const title = text("h1", inSetup ? t("windows.desktop.shared.copy.setupwindowtitle") : routeLabel(activeRoute));
  title.classList.add("settings-pane-title");
  title.tabIndex = -1;

  topbar.append(hamburger, title);
  frame.append(topbar, renderRouteContent(activeRoute, dump));
  pane.append(frame);
  return pane;
}

const DESK_BREATH = {
  audio: t("windows.ui.whilethisisonthesolstoneapptakesinsoundandallofi"),
  screen: t("windows.ui.whilethisisonthesolstoneapptakesinwhatsonyourscr"),
  browsers:
    t("windows.ui.onthesitesyouallowthesolstoneextensiontakesinthe"),
} as const;

function showsDesktopShell(dump: HealthDump | null): boolean {
  if (!dump || label !== "settings") return false;
  if (activeRoute !== "home") return false;
  if (dump.setup?.sequence_enabled !== true || dump.setup.wizard !== "done") return false;
  return !settingsShowsSetupFrame(dump);
}

function ensureShellCss(): void {
  if (document.getElementById("desk-shell-css")) return;
  const style = document.createElement("style");
  style.id = "desk-shell-css";
  style.textContent = shellCss;
  document.head.append(style);
}

function watchCaption(ground: HTMLElement): void {
  captionWatch?.disconnect();
  captionWatch = new MutationObserver(() => reportCaption());
  // The ground hex can stay put while the glow at the top edge moves. Every
  // paint writes a style on the sun or a glow, so the subtree is the signal.
  captionWatch.observe(ground, {
    attributes: true,
    attributeFilter: ["style"],
    subtree: true,
  });
  reportCaption();
}

function reportCaption(): void {
  const ground = document.getElementById("setup-sun");
  if (!ground || ground.hidden || !setupSun?.getFrame) return;
  publishCaptionColor(ground, setupSun.getFrame(), (color) => {
    void Promise.resolve(invoke("set_caption_color", { red: color.red, green: color.green, blue: color.blue })).catch(() => {});
  });
}

function maybeRefreshCounters(dump: HealthDump): void {
  if (countersFor === dump) return;
  countersFor = dump;
  void Promise.resolve(invoke<CounterDocument | null>("get_source_counters"))
    .then((doc) => {
      latestCounters = doc ?? null;
      if (latestHealth === dump) requestRerender();
    })
    .catch(() => {
      latestCounters = null;
    });
}

function bindDeskKeys(): void {
  if (deskKeysBound) return;
  deskKeysBound = true;
  document.addEventListener("keydown", onDeskKey);
  window.addEventListener("resize", () => {
    if (latestHealth && showsDesktopShell(latestHealth)) requestRerender();
  });
}

function onDeskKey(event: KeyboardEvent): void {
  if (!latestHealth || !showsDesktopShell(latestHealth)) return;
  if (event.ctrlKey && event.key === ",") {
    if (isTextEntryTarget(event.target)) return;
    event.preventDefault();
    deskRow = "thisPc";
    deskPane = "status";
    deskPhonePane = true;
    rerender();
    return;
  }
  const tiles = [...document.querySelectorAll<HTMLButtonElement>(".desk-tile-open")];
  const index = tiles.indexOf(document.activeElement as HTMLButtonElement);
  if (index < 0) return;
  if (event.key === "ArrowRight" || event.key === "ArrowDown") {
    event.preventDefault();
    tiles[(index + 1) % tiles.length]?.focus();
    return;
  }
  if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
    event.preventDefault();
    tiles[(index - 1 + tiles.length) % tiles.length]?.focus();
    return;
  }
  if (event.key !== " " && event.key !== "Spacebar") return;
  const id = tiles[index].closest("[data-tile]")?.getAttribute("data-tile");
  if (!id || id === "addMore") return;
  const input = tiles[index].closest("[data-tile]")?.querySelector("input");
  const switchOn = input instanceof HTMLInputElement ? input.checked : null;
  const action = spaceOnTile(id as SourceId, id === "browsers" ? null : switchOn);
  if (action === "none") return;
  event.preventDefault();
  if (action === "open") {
    openDeskSource(id);
    return;
  }
  if (input instanceof HTMLInputElement) {
    input.checked = !input.checked;
    input.dispatchEvent(new Event("change"));
  }
}

function sourceFaulted(dump: HealthDump, kind: SourceKind): boolean {
  const report = sourceByKind(dump, kind);
  return Boolean(report && report.status === "faulted" && !isAccessDenied(report) && isSourceEnabled(dump.setup, kind));
}

function deskPausedLine(dump: HealthDump): string | null {
  if (!globalPauseActive(dump)) return null;
  const secs = dump.pause?.seconds_remaining;
  const remaining =
    secs == null || healthReceivedAt == null ? null : Math.max(0, secs - (nowSecs() - healthReceivedAt));
  const left = pauseTimeLeft(dump.pause ? remaining : null);
  if (left.kind === "untilResume") return t("windows.ui.pauseduntilyouresume");
  if (left.kind === "lessThanAMinute") return t("windows.ui.pausedlessthanaminuteleft");
  return t("windows.ui.pausedarg0minleft", { "arg0": String(left.minutes) });
}

function deskJournalInput(dump: HealthDump): JournalPillInput {
  const phase = dump.sync.pairing.phase;
  const waiting = (dump.sync.upload.pending_segments ?? 0) + (latestImport?.waiting ?? dump.sync.imports_waiting ?? 0);
  const link = dump.sync.journal_link;
  const paired = phase === "paired" || phase === "awaiting_confirmation" || phase === "pairing";
  return {
    paired,
    needsNewLink: false,
    refused: false,
    syncPaused: false,
    held: phase === "awaiting_confirmation",
    offline: phase === "paired" && link === "offline",
    connecting: phase === "pairing" || (phase === "paired" && link === "connecting"),
    waitingToSync: phase === "paired" && waiting > 0 && link !== "offline" && link !== "connecting",
    waiting,
    sourceNeedsAttention: false,
    importsNeedOwner: latestImport?.needs_owner ?? dump.sync.imports_need_owner ?? false,
  };
}

function deskFacts(dump: HealthDump): { label: string; value: string }[] {
  const phase = dump.sync.pairing.phase;
  const paired = phase === "paired" || phase === "awaiting_confirmation";
  const link = dump.sync.journal_link;
  const synced = dump.sync.upload.last_successful_sync;
  const lastReached = typeof synced === "number" ? lastSyncedLabel(synced, nowSecs()) : null;
  const word = link === "connected" || link === "connecting" || link === "offline" ? link : null;
  return journalFacts({ paired, link: word, lastReached });
}

function deskSourceName(id: string): string {
  if (id === "obsidian") return "Obsidian";
  if (id === "addMore") return t("windows.ui.addmore");
  if (id === "audio") return t("windows.ui.audio");
  if (id === "screen") return t("windows.ui.screen");
  if (id === "browsers") return t("windows.ui.browsers");
  if (id === "screenshots") return t("windows.ui.screenshots");
  return id;
}

function totalWords(source: SourceId, total: number): string {
  if (source === "audio" || source === "screen") return durationTotal(total);
  if (source === "browsers") return quantity("pages", total);
  if (source === "screenshots") return SCREENSHOTS_COPY.S20(total);
  return quantity("notes", total);
}

function factFor(source: SourceId): SourceFacts | undefined {
  const facts = counterFacts(latestCounters ?? null);
  return facts?.find((fact) => fact.source === source);
}

function openDeskSource(id: string): void {
  deskRow = "home";
  deskPane = id === "addMore" ? "addMore" : (id as Pane);
  deskPhonePane = true;
  deskShelfOpen = false;
  rerender();
}

function onDeskSwitch(id: string, on: boolean): void {
  if (id === "audio") {
    void invoke("set_audio_tile", { on }).then(() => retryHealth());
    return;
  }
  if (id === "screen") {
    void invoke("set_source_enabled", { kind: "screen", enabled: on }).then(() => retryHealth());
    return;
  }
  if (id === "screenshots") {
    if (on) {
      openDeskSource("screenshots");
      // With no folder to read, the page asks for one; the switch does nothing more.
      if (latestScreenshots?.state !== "no_folder") screenshotsCommand("screenshots_turn_on");
    } else {
      screenshotsCommand("screenshots_turn_off");
    }
    return;
  }
  if (id === "obsidian") {
    if (on) {
      obsidianWantsFolder = true;
      openDeskSource("obsidian");
    } else {
      obsidianWantsFolder = false;
      void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_turn_off"));
    }
  }
}

function deskTilesFor(dump: HealthDump): DeskTile[] {
  const paused = globalPauseActive(dump);
  const setup = dump.setup;
  const mic = sourceByKind(dump, "mic");
  const sound = sourceByKind(dump, "system_audio");
  const screen = sourceByKind(dump, "screen");
  const activity = sourceByKind(dump, "desktop_activity");
  const audio = audioTile({
    micEnabled: setup?.mic === true,
    micAccessDenied: isAccessDenied(mic),
    micDevicePresent: mic?.status !== "no_input_device",
    micTakingIn: mic?.status === "active",
    systemAudioEnabled: setup?.system_audio === true,
    systemAudioFaulted: Boolean(sound && sound.status === "faulted" && !isAccessDenied(sound)),
    systemAudioTakingIn: sound?.status === "active",
    appPaused: paused,
    wanted: setup?.system_audio_wanted ?? null,
  });
  const screenView = screenTile({
    enabled: setup?.screen === true,
    accessDenied: isAccessDenied(screen),
    failed: Boolean(screen && screen.status === "faulted" && !isAccessDenied(screen)),
    activityFailed: Boolean(activity && activity.status === "faulted" && !isAccessDenied(activity) && setup?.desktop_activity === true),
    activityOn: setup?.desktop_activity === true,
    activityRunning: activity?.status === "active",
    appPaused: paused,
    running: screen?.status === "active",
    permissionAsked: true,
  });
  const browsers = browsersTile({
    capture: latestBrowser?.capture ?? "unavailable",
    deliveryFailed: latestBrowser?.delivery === "failed",
    appPaused: paused,
  });
  if (latestObsidian && latestObsidian.state !== "off") obsidianWantsFolder = false;
  const obsidian = obsidianTile({
    state: latestObsidian?.state ?? "off",
    vault: latestObsidian?.vault ?? null,
    foundCount: obsidianFound.length,
    armed: obsidianWantsFolder,
  });
  const screenshots = screenshotsTile(latestScreenshots?.state ?? null);
  const screenshotsLines = screenshotsTileLine(latestScreenshots);
  const browserNames = latestBrowser
    ? [...new Set(latestBrowser.connected.map((item) => BROWSER_NAMES[item.brand] ?? item.brand))]
    : [];
  return deckTiles().map((id) => {
    if (id === "import") {
      return { id, name: t("windows.ui.import"), word: "", sub: t("windows.ui.files"), switchOn: null, switchId: null, switchLabel: "", selected: deskPane === "import" };
    }
    if (id === "addMore") {
      return {
        id,
        name: t("windows.ui.addmore"),
        word: "",
        sub: t("windows.desktop.shared.app.sourcestitle"),
        switchOn: null,
        switchId: null,
        switchLabel: "",
        selected: deskPane === "addMore",
      };
    }
    const view =
      id === "audio"
        ? audio
        : id === "screen"
          ? screenView
          : id === "browsers"
            ? browsers
            : id === "screenshots"
              ? screenshots
              : obsidian;
    const paired = dump.sync.pairing.phase === "paired" || dump.sync.pairing.phase === "awaiting_confirmation";
    const companion =
      id === "audio"
        ? audioSubline({
            systemAudioOn: setup?.system_audio === true,
            word: audio.word,
            microphone: audio.namesMicrophone ? mic?.device || t("windows.ui.microphone") : null,
          })
        : id === "screen"
          ? screenView.activityKept
            ? t("windows.ui.activityisstillon")
            : screenView.activityAlongside
              ? t("windows.ui.withactivity")
              : ""
          : id === "browsers"
            ? browserNames.join(", ")
            : id === "screenshots"
              ? screenshotsLines.companion
              : obsidian.word === "on" || obsidian.word === "paused"
              ? latestObsidian?.vault ?? ""
              : "";
    const setupLine =
      id === "browsers"
        ? BROWSER_TILE_SETUP
        : id === "screenshots"
          ? screenshotsLines.setup
          : id === "obsidian"
          ? obsidianFound.length === 0
            ? OBSIDIAN_TILE_SETUP
            : OBSIDIAN_COPY.foundLabel
          : "";
    const attention =
      id === "audio"
        ? audioTileAttention(dump)
        : id === "screen" && (isAccessDenied(screen) || isAccessDenied(activity))
          ? ACCESS_DENIED_TILE
          : id === "browsers"
            ? browsers.word === "needs attention"
              ? latestBrowser?.capture === "intake_off"
                ? BROWSER_COPY.held
                : BROWSER_COPY.failed
              : ""
            : id === "obsidian" && latestObsidian?.attention
              ? obsidianAttentionText(latestObsidian.attention)
              : id === "screenshots"
                ? screenshotsLines.attention
                : "";
    const sub = sourceSubline({
      word: view.word,
      paired,
      companion,
      setup: setupLine,
      attention,
      settingUp: id === "obsidian" ? obsidianSettingUpLine(latestObsidian) : undefined,
    });
    return {
      id,
      name: deskSourceName(id),
      word: view.word,
      sub,
      switchOn: view.switchOn,
      switchId:
        id === "screen"
          ? ids["settings.sources.screen.enabled"]
          : id === "screenshots"
            ? ids["settings.sources.screenshots.enabled"]
            : null,
      switchLabel: deskSourceName(id),
      selected: deskPane === id,
    };
  });
}

function deskStatusRows(): DeskStatusRow[] {
  const now = nowSecs();
  const rows: DeskStatusRow[] = (["audio", "screen", "browsers", "obsidian", "screenshots"] as const).map((id) => {
    const fact = factFor(id);
    if (!fact) return { id, name: deskSourceName(id), nothingYet: false, total: "", relative: "", hasFacts: false };
    return {
      id,
      name: deskSourceName(id),
      nothingYet: fact.nothingYet,
      total: fact.nothingYet ? "" : totalWords(id, fact.total),
      relative: fact.last === null ? "" : relativeTaken(fact.last, now),
      hasFacts: true,
    };
  });
  rows.push({ id: "import", name: t("windows.ui.import"), nothingYet: false, total: latestImport?.recent ?? "", relative: "", hasFacts: latestImport !== null });
  return rows;
}

function deskSection(title: string): HTMLElement {
  const node = text("h3", title);
  node.className = "desk-section";
  return node;
}

function deskFactBlock(source: SourceId): HTMLElement | null {
  const fact = factFor(source);
  if (!fact) return null;
  const block = document.createElement("div");
  block.className = "desk-facts";
  const soFar = document.createElement("div");
  soFar.className = "desk-fact";
  const soFarLabel = text("span", t("windows.ui.sofar"));
  soFarLabel.className = "desk-fact-label";
  const soFarValue = text(
    "span",
    fact.nothingYet
      ? t("windows.ui.nothingyet")
      : `${totalWords(source, fact.total)}, since ${startedLabel(fact.started ?? "", new Date().getFullYear())}`,
  );
  soFarValue.className = "desk-fact-value";
  soFar.append(soFarLabel, soFarValue);
  const last = document.createElement("div");
  last.className = "desk-fact";
  const lastLabel = text("span", t("windows.ui.lasttakenin"));
  lastLabel.className = "desk-fact-label";
  const lastValue = text("span", fact.last === null ? t("windows.ui.nothingyet") : relativeTaken(fact.last, nowSecs()));
  lastValue.className = "desk-fact-value";
  last.append(lastLabel, lastValue);
  block.append(soFar, last);
  return block;
}

function deskJournalName(dump: HealthDump): string {
  const phase = dump.sync.pairing.phase;
  if (phase === "paired" || phase === "awaiting_confirmation") {
    const mark = dump.sync.pairing.mark;
    if (!mark) return t("windows.ui.markunavailablex5acbd11e");
    return `${mark.words[0].toLowerCase()} \u00B7 ${mark.words[1].toLowerCase()}`;
  }
  return t("windows.ui.yourjournal");
}

function deskJournalChips(dump: HealthDump): { hex: string; rotated: boolean }[] | null {
  const phase = dump.sync.pairing.phase;
  if (phase !== "paired" && phase !== "awaiting_confirmation") return null;
  const mark = dump.sync.pairing.mark;
  if (!displayableMark(mark)) return null;
  return [mark.icon1, mark.icon2].map((icon) => ({ hex: icon.color.hex, rotated: icon.rot === 45 }));
}

function deskByteLabel(bytes: number): string {
  if (bytes <= 0) return t("windows.ui.nothingyet");
  const mb = 1_000_000;
  const kb = 1_000;
  if (bytes < mb) return t("windows.ui.arg0kb", { "arg0": String(Math.max(1, Math.floor(bytes / kb))) });
  return t("windows.ui.arg0mb", { "arg0": String(Math.floor(bytes / mb)) });
}

function deskKept(dump: HealthDump): { label: string; value: string } | null {
  const phase = dump.sync.pairing.phase;
  if (phase === "paired" || phase === "awaiting_confirmation") return null;
  const bytes = latestStorage?.bytes;
  if (typeof bytes !== "number") return null;
  return { label: t("windows.ui.keptonthispc"), value: deskByteLabel(bytes) };
}

function deskTitleGlyph(): GlyphName | null {
  if (deskRow !== "home") return null;
  if (
    deskPane === "audio" ||
    deskPane === "screen" ||
    deskPane === "browsers" ||
    deskPane === "obsidian" ||
    deskPane === "screenshots" ||
    deskPane === "import" ||
    deskPane === "addMore"
  ) {
    return glyphForTile(deskPane);
  }
  return null;
}

function restartControl(): HTMLButtonElement {
  return actionButton(t("windows.ui.restartintake"), ids["tray.menu.restartObserving"], true, () => {
    void invoke("resume").then(() => retryHealth());
  });
}

function deskRestart(dump: HealthDump, place: "audio" | "screen" | "technicalDetails"): HTMLElement | null {
  const where = restartPlace({
    phaseError: dump.app_state === "error",
    micFaulted: sourceFaulted(dump, "mic"),
    systemAudioFaulted: sourceFaulted(dump, "system_audio"),
    screenFaulted: sourceFaulted(dump, "screen"),
    activityFaulted: sourceFaulted(dump, "desktop_activity"),
    storageFault: dump.app_state === "error",
  });
  return where === place ? restartControl() : null;
}

function breath(sentence: string): HTMLElement {
  const node = text("p", sentence);
  node.classList.add("desk-breath");
  return node;
}

function deskReason(sentence: string): HTMLElement {
  const node = text("p", sentence);
  node.classList.add("desk-reason");
  return node;
}

function deskAction(labelText: string, onClick: () => void, automationId?: string): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "desk-connect";
  button.textContent = labelText;
  if (automationId) button.dataset.automationId = automationId;
  button.onclick = onClick;
  return button;
}

function deskSwitchRow(
  labelText: string,
  on: boolean,
  sentence: string | null,
  change: (on: boolean) => void,
): HTMLElement {
  const row = document.createElement("div");
  row.className = "desk-switch-row";
  const head = document.createElement("div");
  head.className = "desk-switch-head";
  const control = document.createElement("label");
  control.className = "desk-switch";
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = on;
  input.setAttribute("aria-label", labelText);
  input.onchange = () => change(input.checked);
  control.append(input);
  head.append(text("span", labelText), control);
  row.append(head);
  if (sentence) {
    const note = text("p", sentence);
    note.className = "desk-switch-note";
    row.append(note);
  }
  return row;
}

function deskChild(labelText: string, value: string): HTMLElement {
  const row = document.createElement("div");
  row.className = "desk-fact";
  const label = text("span", labelText);
  label.className = "desk-fact-label";
  const mark = text("span", value);
  mark.className = "desk-fact-value";
  row.append(label, mark);
  return row;
}

function audioDiagnosis(dump: HealthDump): string {
  const mic = sourceByKind(dump, "mic");
  const sound = sourceByKind(dump, "system_audio");
  if (isAccessDenied(mic) || isAccessDenied(sound)) return ACCESS_DENIED_LINE;
  if (mic?.status === "no_input_device") return NO_MICROPHONE_LINE;
  if (sound?.status === "no_input_device") return NO_OUTPUT_LINE;
  return "";
}

function audioTileAttention(dump: HealthDump): string {
  const mic = sourceByKind(dump, "mic");
  const sound = sourceByKind(dump, "system_audio");
  if (isAccessDenied(mic) || isAccessDenied(sound)) return ACCESS_DENIED_TILE;
  if (mic?.status === "no_input_device") return NO_MICROPHONE_TILE;
  if (sound?.status === "no_input_device") return NO_OUTPUT_TILE;
  return "";
}

function deskMicChildren(): HTMLElement | null {
  if (!latestMic) return null;
  const ordered = orderedMicDevices(latestMic.config, micDevices);
  if (ordered.length === 0) return null;
  const block = document.createElement("div");
  block.append(deskSection(t("windows.ui.microphones")));
  for (const device of ordered) {
    block.append(deskChild(device.name, device.id === latestMic.active_id ? t("windows.ui.activex96879611") : ""));
  }
  const gain = document.createElement("div");
  gain.className = "desk-switch-row";
  const head = document.createElement("div");
  head.className = "desk-switch-head";
  head.append(text("span", t("windows.ui.inputgain")));
  const levels = document.createElement("div");
  levels.className = "desk-gain";
  levels.setAttribute("role", "group");
  levels.setAttribute("aria-label", t("windows.ui.inputgain"));
  for (const level of MIC_GAIN_LEVELS) {
    const on = latestMic.config.gain === level;
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = t("windows.ui.textcontent5", { "level": String(level) });
    button.className = on ? "desk-connect" : "desk-gain-level";
    button.onclick = () => {
      if (!latestMic) return;
      void applyMic({ ...latestMic.config, gain: level });
    };
    levels.append(button);
  }
  head.append(levels);
  gain.append(head);
  block.append(gain);
  return block;
}

function deskBrowserChildren(status: BrowserStatus): HTMLElement | null {
  if (status.connected.length === 0) return null;
  const block = document.createElement("div");
  block.append(deskSection(t("windows.ui.browsers")));
  const names = [...new Set(status.connected.map((item) => BROWSER_NAMES[item.brand] ?? item.brand))];
  for (const name of names) block.append(deskChild(name, t("windows.ui.connected")));
  return block;
}


async function refreshImportStatus(): Promise<void> {
  if (label !== "settings") return;
  const next = await invoke<ImportDocument | null>("import_status").catch(() => null);
  if (JSON.stringify(next) !== JSON.stringify(latestImport)) {
    latestImport = next;
    requestRerender();
  }
}

async function importAction(command: string, args?: Record<string, unknown>): Promise<void> {
  importActionError = null;
  try { await invoke(command, args); } catch { importActionError = t("windows.ui.thisstepcouldntfinishtryagain"); }
  await refreshImportStatus();
  requestRerender();
}

async function askRemoveImport(row: ImportRow): Promise<void> {
  const prompt = await invoke<{ name: string; may_have_reached: boolean; missing_copy: boolean } | null>("import_prepare_remove", { id: row.id }).catch(() => null);
  if (!prompt) { await refreshImportStatus(); return; }
  const dialog = document.createElement("dialog");
  dialog.className = "desk-import-confirm";
  const shell = document.querySelector(".desk-shell");
  if (shell) {
    const tokens = getComputedStyle(shell);
    for (const name of ["--desk-ink", "--desk-card", "--desk-hairline"]) dialog.style.setProperty(name, tokens.getPropertyValue(name));
  }
  const title = text("h2", t("windows.ui.removearg0", { "arg0": String(prompt.name) }));
  title.id = "import-remove-title";
  dialog.setAttribute("aria-labelledby", title.id);
  const reason = text("p", prompt.missing_copy
    ? t("windows.ui.thecopyisalreadygonesothisonlyclearsitfromhere")
    : prompt.may_have_reached
    ? t("windows.ui.thisremovesthecopyonthisdeviceyourjournalmaystil")
    : t("windows.ui.thisremovesthecopyonthisdevicesoitwontreachyourj"));
  const actions = document.createElement("div");
  actions.className = "desk-actions";
  const cancel = deskAction(t("windows.desktop.shared.setup.cancel"), () => dialog.close());
  const remove = deskAction(t("windows.ui.removex7e5608ab"), () => {
    void invoke("import_remove", { id: row.id }).then(() => {
      dialog.close();
      void refreshImportStatus();
    }).catch(() => {
      reason.textContent = t("windows.ui.couldntremovethistryagain");
    });
  });
  actions.append(cancel, remove);
  dialog.append(title, reason, actions);
  dialog.addEventListener("close", () => {
    void invoke("import_cancel_remove", { id: row.id }).catch(() => undefined);
    dialog.remove();
  }, { once: true });
  document.body.append(dialog);
  dialog.showModal();
  cancel.focus();
}

function renderImportPage(): HTMLElement {
  const body = document.createElement("div");
  body.className = "desk-import-page";
  body.append(breath(t("windows.ui.addspdfsaudioimagesandtextyouchoosehereordropont")));
  const choose = deskAction(t("windows.ui.choosefiles"), () => { void importAction("import_choose_files"); }, ids["settings.import.choose"]);
  choose.disabled = latestImport?.available === false;
  body.append(choose);
  const intake = latestImport?.intake;
  if (intake) {
    const line = intake.copying
      ? t("windows.ui.savingonthisdevicearg0ofarg1", { "arg0": String(Math.min(intake.done + 1, intake.total)), "arg1": String(intake.total) })
      : intake.accepted === 0 ? t("windows.ui.nothingwassavedonthisdevice") : t("windows.ui.savedonthisdevicearg0", { "arg0": String(intake.accepted) });
    const result = deskReason(line);
    result.setAttribute("aria-live", "polite");
    body.append(result);
    for (const refused of intake.refused) body.append(deskReason(t("windows.ui.text6", { "status": String(refused.name), "reason": String(refused.reason) })));
  }
  if (importActionError) body.append(deskReason(importActionError));
  if (latestImport?.interrupted) {
    body.append(deskReason(t("windows.ui.somethingyouwereaddingdidntfinishsavingadditagai")));
    body.append(deskAction(t("windows.desktop.shared.settings.severity"), () => { void importAction("import_dismiss_interrupted"); }));
  }
  body.append(deskSection(t("windows.ui.recent")));
  if (latestImport) {
    const recent = deskReason(latestImport.recent);
    recent.setAttribute("aria-live", "polite");
    body.append(recent);
  }
  if (latestImport?.rows.length) {
    body.append(deskSection(t("windows.ui.onthisdevice")));
    for (const row of latestImport.rows) {
      const item = document.createElement("div");
      item.className = "desk-import-item";
      item.dataset.importId = row.id;
      item.append(text("strong", row.name));
      item.append(deskReason(row.state === "refused" ? row.reason ?? t("windows.ui.yourjournalcouldntacceptthis") : stateWord(row.state)));
      if (row.state !== "sending") {
        const actions = document.createElement("div");
        actions.className = "desk-actions";
        if (row.state === "refused" && row.retryable) {
          const retry = deskAction(t("windows.ui.trynow"), () => { void importAction("import_retry", { id: row.id }); });
          retry.setAttribute("aria-label", t("windows.ui.trynowarg0", { "arg0": String(row.name) }));
          actions.append(retry);
        }
        const remove = deskAction(t("windows.ui.removex7e5608ab"), () => { void askRemoveImport(row); });
        remove.setAttribute("aria-label", t("windows.ui.remove", { "value": String(row.name) }));
        actions.append(remove);
        item.append(actions);
      }
      body.append(item);
    }
  }
  return body;
}

function deskPaneExtra(dump: HealthDump): HTMLElement | null {
  if (deskRow !== "home") return null;
  if (homeShowsPairing(dump) && deskPane === "status") return renderRouteContent("journal", dump);
  if (deskPane === "status") return null;
  const body = document.createElement("div");
  if (deskPane === "import") return renderImportPage();
  if (deskPane === "technical") {
    body.append(renderStatusStrip(dump));
    const restart = deskRestart(dump, "technicalDetails");
    if (restart) body.append(restart);
    return body;
  }
  if (deskPane === "audio") {
    const tiles = deskTilesFor(dump);
    const word = tiles.find((tile) => tile.id === "audio")?.word;
    if (word === "needs attention") {
      const diagnosis = audioDiagnosis(dump);
      if (diagnosis) body.append(deskReason(diagnosis));
      const mic = sourceByKind(dump, "mic");
      // A refusal opens microphone privacy settings. A missing device opens the sound page.
      if (isAccessDenied(mic)) {
        body.append(
          deskAction(t("windows.ui.openwindowssettings"), () => {
            void invoke("open_microphone_privacy_settings").catch(() => undefined);
          }, ids["settings.sources.mic.permission"]),
        );
      } else if (diagnosis === NO_MICROPHONE_LINE || diagnosis === NO_OUTPUT_LINE) {
        body.append(
          deskAction(t("windows.ui.opensoundsettings"), () => {
            void invoke("open_sound_settings").catch(() => undefined);
          }),
        );
      }
    }
    body.append(breath(DESK_BREATH.audio));
    const facts = deskFactBlock("audio");
    if (facts) body.append(facts);
    body.append(deskSection(t("windows.ui.goesalongwithit")));
    const wanted = derivedComputerSoundWanted(
      dump.setup?.mic === true,
      dump.setup?.system_audio === true,
      dump.setup?.system_audio_wanted ?? null,
    );
    body.append(
      deskSwitchRow(t("windows.ui.computersound"), wanted, null, (on) => {
        void invoke("set_computer_sound", { wanted: on }).catch(() => undefined).then(() => retryHealth());
      }),
    );
    const mics = deskMicChildren();
    if (mics) body.append(mics);
    const restart = deskRestart(dump, "audio");
    if (restart) body.append(restart);
    return body;
  }
  if (deskPane === "screen") {
    const screenReport = sourceByKind(dump, "screen");
    const activityReport = sourceByKind(dump, "desktop_activity");
    if (isAccessDenied(screenReport) || isAccessDenied(activityReport)) {
      body.append(deskReason(ACCESS_DENIED_LINE));
    }
    body.append(breath(DESK_BREATH.screen));
    const facts = deskFactBlock("screen");
    if (facts) body.append(facts);
    body.append(deskSection(t("windows.ui.goesalongwithit")));
    body.append(
      deskSwitchRow(t("windows.ui.activity"), dump.setup?.desktop_activity_allowed === true, ACTIVITY_SENTENCE, (on) => {
        void invoke("set_source_enabled", { kind: "desktop_activity", enabled: on }).catch(() => undefined).then(() => retryHealth());
      }),
    );
    body.append(
      deskSwitchRow(t("windows.desktop.shared.settings.windowtitles"), dump.setup?.desktop_activity_window_titles === true, null, (on) => {
        void invoke("set_desktop_activity_window_titles", { enabled: on }).catch(() => undefined).then(() => retryHealth());
      }),
    );
    body.append(
      deskSwitchRow(t("windows.ui.pageaddresses"), dump.setup?.desktop_activity_page_addresses === true, null, (on) => {
        void invoke("set_desktop_activity_page_addresses", { enabled: on }).catch(() => undefined).then(() => retryHealth());
      }),
    );
    body.append(
      deskSwitchRow(
        t("windows.ui.keeponwhilepausedorscreenis"),
        dump.setup?.desktop_activity_through_pause === true,
        null,
        (on) => {
          void invoke("set_desktop_activity_through_pause", { enabled: on }).catch(() => undefined).then(() => retryHealth());
        },
      ),
    );
    body.append(deskSection(t("windows.ui.leftout")));
    const shared = text("p", t("windows.ui.thislistappliestoactivitytoo"));
    shared.classList.add("desk-breath");
    body.append(shared);
    if (latestExclusions) body.append(renderExclusionsSection(latestExclusions, dump, true));
    const restart = deskRestart(dump, "screen");
    if (restart) body.append(restart);
    return body;
  }
  if (deskPane === "browsers") {
    const capture = latestBrowser?.capture;
    if (capture === "not_paired" || capture === "unavailable" || !latestBrowser) {
      body.append(deskReason(BROWSER_COPY.setup));
      if (latestBrowser && latestBrowser.stores.length > 0) {
        const stores = document.createElement("div");
        stores.className = "desk-actions";
        for (const brand of latestBrowser.stores) {
          stores.append(
            deskAction(t("windows.ui.addto", { "value": String(BROWSER_NAMES[brand] ?? brand) }), () => {
              void openBrowserStore(brand);
            }),
          );
        }
        body.append(stores);
      }
    } else if (capture === "intake_off" || latestBrowser.delivery === "failed") {
      body.append(deskReason(capture === "intake_off" ? BROWSER_COPY.held : BROWSER_COPY.failed));
    }
    body.append(breath(DESK_BREATH.browsers));
    const facts = deskFactBlock("browsers");
    if (facts) body.append(facts);
    if (latestBrowser) {
      const children = deskBrowserChildren(latestBrowser);
      if (children) body.append(children);
    }
    return body;
  }
  if (deskPane === "screenshots") return renderScreenshotsPage(body);
  if (deskPane === "obsidian") {
    const status = latestObsidian;
    const ready = status?.state === "off" && obsidianWantsFolder && !status.vault;
    if (status?.state === "needs_attention" && status.attention) {
      body.append(automation(deskReason(obsidianAttentionText(status.attention)), ids["settings.sources.obsidian.attention"]));
    }
    if (status?.state === "confirm" && status.warning) {
      body.append(automation(deskReason(obsidianWarningText(status.warning)), ids["settings.sources.obsidian.warning"]));
      const answer = document.createElement("div");
      answer.className = "desk-actions";
      const go = deskAction(OBSIDIAN_COPY.continue, () => {
        void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_continue_warning"));
      }, ids["settings.sources.obsidian.continue"]);
      const stop = deskAction(OBSIDIAN_COPY.cancel, () => {
        void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_cancel_warning"));
      }, ids["settings.sources.obsidian.cancel"]);
      go.disabled = obsidianBusy;
      stop.disabled = obsidianBusy;
      answer.append(go, stop);
      body.append(answer);
    }
    if (status?.state === "waiting_for_journal_update") body.append(deskReason(OBSIDIAN_COPY.waitingUpdateDetail));
    if (status?.first_sync) body.append(deskReason(obsidianProgressText(status.first_sync)));
    if (status && status.waiting > 0 && status.state !== "off" && status.state !== "confirm") {
      body.append(deskReason(t("windows.ui.waitingsync", { "value": String(status.waiting) })));
    }
    if (ready) {
      if (obsidianFound.length === 0) body.append(deskReason(OBSIDIAN_COPY.noneFound));
      body.append(
        deskAction(OBSIDIAN_COPY.chooseFolder, () => {
          obsidianRefusal = null;
          void applyObsidianDocument(invoke<ObsidianDocument | null>("obsidian_choose_folder"));
        }, ids["settings.sources.obsidian.choose"]),
      );
      if (obsidianRefusal !== null || obsidianFailed) {
        body.append(
          deskReason(obsidianRefusal !== null ? obsidianRefusalText(obsidianRefusal) : OBSIDIAN_COPY.couldNotTurnOn),
        );
      }
    }
    body.append(breath(OBSIDIAN_COPY.explain));
    const facts = deskFactBlock("obsidian");
    if (facts) body.append(facts);
    const vaultFacts = status ? obsidianDeskStatus(status) : null;
    if (vaultFacts) body.append(vaultFacts);
    if (ready && obsidianFound.length > 0) {
      body.append(deskSection(t("windows.ui.vaults")));
      for (const vault of obsidianFound) {
        const row = document.createElement("div");
        row.className = "desk-switch-row";
        const head = document.createElement("div");
        head.className = "desk-switch-head";
        head.append(text("span", vault.name));
        head.append(
          deskAction(OBSIDIAN_COPY.use, () => {
            obsidianRefusal = null;
            void applyObsidianDocument(
              invoke<ObsidianDocument | null>("obsidian_use_found_vault", { path: vault.path }),
            );
          }),
        );
        row.append(head);
        body.append(row);
      }
    }
    return body;
  }
  if (deskPane === "addMore") {
    body.append(text("h3", t("windows.ui.alreadyonhome")));
    for (const source of addMoreSources()) body.append(text("div", deskSourceName(source)));
    return body;
  }
  if (deskPane === "screenLeftOut") {
    body.append(latestExclusions ? renderExclusionsSection(latestExclusions, dump, true) : loadingCaption());
    return body;
  }
  return null;
}

function deskShelfBody(dump: HealthDump): HTMLElement | null {
  if (deskRow === "journal") return renderRouteContent("journal", dump);
  if (deskRow === "thisPc") {
    const body = document.createElement("div");
    body.append(latestHotkey ? renderHotkeySection(latestHotkey) : loadingCaption());
    const location = document.createElement("div");
    location.append(storageRow(latestStorage));
    body.append(location, renderStartAtLoginControl(dump));
    return body;
  }
  if (deskRow === "updates") return latestUpdate ? renderUpdatesSection(latestUpdate) : loadingCaption();
  if (deskRow === "help") {
    const body = renderRouteContent("help", dump);
    body.append(
      actionButton(t("windows.desktop.shared.settings.gethelp"), undefined, true, () => void invoke("open_help")),
      actionButton(t("windows.desktop.shared.settings.reportaproblem"), undefined, true, () => void invoke("open_support_report")),
    );
    return body;
  }
  return null;
}

function deskPaneTitle(): string {
  if (deskRow === "journal") return t("windows.desktop.shared.copy.setupjournalcardheading");
  if (deskRow === "thisPc") return t("windows.ui.thispc");
  if (deskRow === "updates") return t("windows.desktop.shared.settings.updates");
  if (deskRow === "help") return t("windows.desktop.shared.settings.help");
  if (deskPane === "status") return t("windows.ui.status");
  if (deskPane === "technical") return t("windows.desktop.shared.copy.technicaldetails");
  if (deskPane === "addMore") return t("windows.ui.addmore");
  if (deskPane === "screenLeftOut") return t("windows.ui.leftout");
  return deskSourceName(deskPane);
}

function renderDesktopShell(dump: HealthDump): HTMLElement {
  ensureShellCss();
  const width = widthClass(window.innerWidth || 1180);
  const saveFailed = Boolean(
    (dump.setup?.failed_sources && dump.setup.failed_sources.length > 0) ||
      dump.setup?.desktop_activity_through_pause_save_failed ||
      dump.setup?.desktop_activity_window_titles_save_failed ||
      dump.setup?.desktop_activity_page_addresses_save_failed,
  );
  const retry = saveFailed
    ? actionButton(t("windows.desktop.shared.settings.textcontent"), ids["settings.sources.saveRetry"], true, () => {
        void invoke("retry_save").then(() => retryHealth());
      })
    : null;
  const unread =
    dump.setup?.setup_read_failed === true
      ? (() => {
          const notice = text("div", savedChoicesUnread);
          notice.classList.add("save-failure");
          return notice;
        })()
      : null;
  const updateDisplay = latestUpdate?.display;
  const journal = deskJournalInput(dump);
  const lead = statusLead(journal);
  const pill = journalPill(journal);
  const tiles = deskTilesFor(dump);
  const pageTile =
    deskRow === "home" &&
    (deskPane === "audio" ||
      deskPane === "screen" ||
      deskPane === "browsers" ||
      deskPane === "obsidian" ||
      deskPane === "screenshots")
      ? tiles.find((tile) => tile.id === deskPane) ?? null
      : null;
  const statusOpen = deskRow === "home" && deskPane === "status" && !homeShowsPairing(dump);
  const view = {
    width,
    row: deskRow,
    shelfOpen: deskShelfOpen,
    phoneShowsPane: width === "phone" && deskRow === "home" && deskPhonePane,
    greeting: greeting(new Date().getHours()),
    pill,
    journalName: deskJournalName(dump),
    journalChips: deskJournalChips(dump),
    pausedLine: deskPausedLine(dump),
    badges: shelfBadges({
      unknownJournal: (dump.sync.unknown_journals?.length ?? 0) > 0,
      journalOffline: journal.offline,
      shortcutNeedsRepair: latestHotkey?.registration === "combo_taken" || latestHotkey?.registration === "failed",
      updateAvailable:
        updateDisplay === "available" ||
        updateDisplay === "downloading" ||
        updateDisplay === "staged" ||
        updateDisplay === "failed_with_available",
    }),
    tiles,
    rows: deskStatusRows(),
    paneTitle: deskPaneTitle(),
    titleGlyph: deskTitleGlyph(),
    paneIsStatus: statusOpen,
    connected: pill === t("windows.ui.connected"),
    leadHeadline: lead.headline || pill,
    leadLines: lead.lines,
    kept: statusOpen ? deskKept(dump) : null,
    facts: deskFacts(dump),
    showConnect:
      statusOpen && (dump.sync.pairing.phase === "not_paired" || dump.sync.pairing.phase === "failed"),
    showTechnical: statusOpen,
    shelfPage: deskRow !== "home",
    pageVerdict: pageTile
      ? {
          id: pageTile.id,
          word: pageTile.word,
          switchOn: pageTile.switchOn,
          switchId: pageTile.switchId,
          switchLabel: pageTile.switchLabel,
        }
      : null,
  };
  return renderDesk(
    view,
    {
      notice: dump.setup?.notice_dismissed ? null : renderSourcesNotice(dump),
      unread,
      saveRetry: retry,
      paneExtra: deskPaneExtra(dump),
      shelfBody: deskShelfBody(dump),
    },
    {
      onShelf: (row) => {
        deskRow = row;
        deskShelfOpen = false;
        if (row === "home") deskPhonePane = deskPane !== "status";
        rerender();
      },
      onShelfToggle: () => {
        deskShelfOpen = !deskShelfOpen;
        rerender();
      },
      onBackHome: () => {
        deskRow = "home";
        deskPane = "status";
        deskPhonePane = false;
        deskShelfOpen = false;
        rerender();
      },
      onPill: () => {
        deskRow = "home";
        deskPane = "status";
        deskPhonePane = width === "phone";
        rerender();
      },
      onResume: () => {
        void invoke("resume").then(() => retryHealth());
      },
      onClosePane: () => {
        deskPane = "status";
        deskPhonePane = false;
        rerender();
      },
      onTechnical: () => {
        deskRow = "home";
        deskPane = "technical";
        deskPhonePane = true;
        deskShelfOpen = false;
        rerender();
      },
      onTile: (id) => openDeskSource(id),
      onSwitch: (id, on) => onDeskSwitch(id, on),
      onStatus: (id) => openDeskSource(id),
    },
  );
}

function renderSettingsShell(dump: HealthDump): HTMLElement {
  const shell = document.createElement("div");
  shell.classList.add("settings-shell");
  shell.dataset.paneOpen = "false";

  const scrim = document.createElement("div");
  scrim.classList.add("settings-scrim");

  shell.append(scrim, renderRail(), renderPane(dump));
  return shell;
}

function settingsShowsSetupFrame(dump: HealthDump): boolean {
  return activeRoute === "home" && dump.setup?.sequence_enabled === true && dump.setup.wizard !== "done";
}

// The sun arc behind the setup frame, drawn by the journal's own engine (./brand/sunarc.js).
// It lives outside #app, which every health update rebuilds, and is torn down (its clock and
// listeners stopped) whenever the frame is not showing.
interface SunFrame {
  ground?: string;
  glow?: { x: number; y: number; r: number; a: number; color: string } | null;
  halo?: { x: number; y: number; r: number; a: number; color: string } | null;
}
interface SunArcInstance {
  teardown(): void;
  getFrame?: () => SunFrame | null;
}
let setupSun: SunArcInstance | null = null;

function syncSetupSun(framed: boolean): void {
  let ground = document.getElementById("setup-sun");
  if (setupSun && (!framed || !ground?.isConnected)) {
    setupSun.teardown();
    setupSun = null;
    captionWatch?.disconnect();
    captionWatch = null;
    resetCaptionReport();
    if (ground) ground.hidden = true;
  }
  if (!framed || setupSun) return;
  const engine = (window as unknown as { SunArc?: { mount(el: HTMLElement): SunArcInstance | null } }).SunArc;
  if (!engine || !root.parentNode) return;
  if (!ground) {
    ground = document.createElement("div");
    ground.id = "setup-sun";
    ground.setAttribute("aria-hidden", "true");
    root.before(ground);
  }
  ground.hidden = false;
  setupSun = engine.mount(ground);
  if (!setupSun) {
    ground.hidden = true;
    return;
  }
  watchCaption(ground);
}

function renderSettings(dump: HealthDump): void {
  setLatestHealth(dump);
  const logText = logHelp?.querySelector("textarea");
  const logPosition = logText ? [logText.selectionStart, logText.selectionEnd, logText.scrollTop, logText.scrollLeft] : null;
  resetRoot(ids["settings.window.root"]);
  root.style.padding = "0";
  root.style.overflow = "hidden";
  root.style.boxSizing = "border-box";
  if (startupDestination && dump.setup?.sequence_enabled === true) {
    if (dump.setup.wizard === "done") {
      const next = settingsNavigate(startupDestination);
      deskRow = next.row;
      deskPane = next.pane;
      deskPhonePane = next.row !== "home" || next.pane !== "status";
    }
    startupDestination = null;
  }
  const showFrame = settingsShowsSetupFrame(dump);
  const showDesk = showsDesktopShell(dump);
  root.classList.remove("desk-about");
  root.append(showFrame ? renderSetupFrame(dump) : showDesk ? renderDesktopShell(dump) : renderSettingsShell(dump));
  syncSetupSun(showFrame || showDesk);
  if (showDesk) {
    bindDeskKeys();
    maybeRefreshCounters(dump);
  }
  if (logText && logPosition) {
    logText.setSelectionRange(logPosition[0], logPosition[1]);
    logText.scrollTop = logPosition[2];
    logText.scrollLeft = logPosition[3];
  }
  // The mark is spoken before the question, the first time its card is on screen.
  // The desk draws a ring only after a keyboard focus, so it does not take one here.
  const unfocusedMarkCard = markCardFocused
    ? null
    : root.querySelector<HTMLElement>(`[data-automation-id="${ids["settings.pairing.markCard"]}"]`);
  if (showDesk) {
    focusPaneTitleOnRender = false;
  } else if (focusWizardPrimary) {
    focusWizardPrimary = false;
    focusPaneTitleOnRender = false;
    root.querySelector<HTMLElement>(".wizard-step-title")?.focus();
  } else if (unfocusedMarkCard) {
    focusPaneTitleOnRender = false;
    markCardFocused = true;
    unfocusedMarkCard.focus();
  } else if (focusPaneTitleOnRender) {
    focusPaneTitleOnRender = false;
    const target = showFrame
      ? root.querySelector<HTMLElement>(".wizard-step-title")
      : root.querySelector<HTMLElement>(".settings-pane-title");
    target?.focus();
  } else if (dump.sync?.pairing?.phase !== "awaiting_confirmation") {
    markCardFocused = false;
  }
  openJournalPaneForWaitingLink(dump);
}

function renderAbout(dump: HealthDump): void {
  resetRoot(ids["about.window.root"]);
  ensureShellCss();
  root.classList.add("desk-about");
  syncSetupSun(true);
  root.style.padding = "22px";
  root.style.boxSizing = "border-box";

  const title = text("h1", "solstone");
  title.style.margin = "0 0 12px";
  title.style.fontSize = "24px";

  const body1 = text(
    "p",
    t("windows.desktop.shared.about.setupwelcomebody1"),
  );
  const body2 = text("p", t("windows.desktop.shared.copy.setupwelcomebody2"));
  const body3 = text("p", t("windows.desktop.shared.about.opensourcelocalfirst"));
  const body4 = text("p", t("windows.ui.madebysolpbc"));
  for (const body of [body1, body2, body3, body4]) {
    body.style.margin = "0 0 12px";
    body.style.lineHeight = "1.5";
    body.style.color = "var(--fg-subtle)";
  }
  body4.style.margin = "0 0 18px";

  const version = selectable(
    automation(text("div", dump.sync.about_app_line ?? ""), ids["about.version"]),
  );
  version.classList.add("desk-about-version");

  root.append(title, body1, body2, body3, body4, version);

  const jvEl = selectable(
    automation(
      text("div", aboutJournalValue(dump.sync.journal_display_line ?? t("windows.desktop.shared.about.journalunknown"))),
      ids["about.journalVersion"],
    ),
  );
  const journalRow = document.createElement("div");
  journalRow.className = "desk-about-row";
  const journalLabel = text("div", t("windows.ui.journalversion"));
  journalLabel.className = "desk-about-label";
  jvEl.classList.add("desk-about-version");
  journalRow.append(journalLabel, jvEl);
  root.append(journalRow);

  const copy = document.createElement("button");
  copy.type = "button";
  copy.textContent = t("windows.desktop.shared.about.textcontent");
  copy.style.marginTop = "14px";
  copy.style.padding = "7px 12px";
  copy.style.border = "1px solid var(--border)";
  copy.style.borderRadius = "var(--radius-control)";
  copy.style.background = "var(--surface)";
  copy.style.color = "var(--fg)";
  copy.style.cursor = "pointer";
  copy.onclick = async () => {
    try {
      const block = dump.sync.about_block ?? "";
      if (aboutClipboardWriter) {
        await aboutClipboardWriter(block);
      } else {
        await navigator.clipboard.writeText(block);
      }
      copy.textContent = t("windows.ui.copied");
    } catch {
      copy.textContent = t("windows.desktop.shared.about.textcontent2");
    }
  };
  root.append(copy);
}

function nowSecs(): number {
  return Math.floor(Date.now() / 1000);
}

function setLatestHealth(dump: HealthDump | null): void {
  latestHealth = dump;
  if (openProofKind !== "closed" && !proofEligible(openProofKind, dump)) closeProofConsumer();
  healthReceivedAt = dump ? nowSecs() : null;
}

// Mirror of observer_update::last_checked_relative (the Rust-tested canonical
// spec, where the <60s "just now" threshold is pinned). This ticks the live clock.
function lastCheckedRelative(checkedAt: number | null, secsNow: number): string {
  if (checkedAt == null) {
    return t("windows.desktop.shared.updates.nevercheckedforupdates");
  }
  const secs = Math.max(0, secsNow - checkedAt);
  if (secs < 60) {
    return t("windows.ui.checkedjustnow");
  }
  if (secs < 3600) {
    const m = Math.floor(secs / 60);
    return t("windows.time.checkedminute", { count: m, display: number(m, { useGrouping: false }) });
  }
  if (secs < 86400) {
    const h = Math.floor(secs / 3600);
    return t("windows.time.checkedhour", { count: h, display: number(h, { useGrouping: false }) });
  }
  const d = Math.floor(secs / 86400);
  return t("windows.time.checkedday", { count: d, display: number(d, { useGrouping: false }) });
}

function updateHeadline(view: UpdateView): string {
  const v = view.available_version ?? "";
  const stagedV = view.downloaded_version ?? "";
  if (view.activity === "installing") {
    return stagedV ? t("windows.ui.installingsolstone", { "v": String(stagedV) }) : t("windows.ui.installing");
  }
  switch (view.display) {
    case "never_checked":
      return t("windows.ui.notcheckedforupdatesyet");
    case "up_to_date":
      return t("windows.desktop.shared.copy.settingstabupdatesdonea11y");
    case "checking":
      return t("windows.ui.checkingforupdates");
    case "available":
      return t("windows.ui.solstoneisavailable", { "v": String(v) });
    case "downloading":
      return t("windows.ui.downloadingsolstone", { "v": String(v) });
    case "staged":
      return stagedV ? t("windows.ui.solstoneisreadytoinstall", { "v": String(stagedV) }) : t("windows.ui.solstonereadyinstall");
    case "failed":
      if (view.recovery === "download") {
        return t("windows.ui.couldntdownloadupdate");
      }
      if (view.recovery === "install") {
        return t("windows.ui.couldntinstallupdate");
      }
      return t("windows.ui.couldntcheckforupdates");
    case "failed_with_available":
      if (view.recovery === "download") {
        return t("windows.ui.couldntdownloadsolstone", { "value": String(v) });
      }
      return t("windows.ui.couldntcheckforupdatessolstonewas", { "v": String(v) });
    case "unavailable":
      return t("windows.ui.thisbuildcantupdateitself");
  }
}

function costDeferralText(deferral: "metered" | "roaming" | "restricted" | "unknown"): string {
  switch (deferral) {
    case "metered":
      return t("windows.ui.downloaddeferredmeteredconnection");
    case "roaming":
      return t("windows.ui.downloaddeferredwhileroaming");
    case "restricted":
      return t("windows.ui.backgrounddatauserestricted");
    case "unknown":
      return t("windows.ui.downloaddeferredconnectionstatusunknown");
  }
}

// The quiet subtitle under the headline. `live` marks the last-checked clock so
// the 1s tick only rewrites those states; static subtitles never tick. Returns
// null where the headline + progress bar already say everything (checking,
// downloading).
function updateSubtitle(
  view: UpdateView,
  secsNow: number,
): { text: string; live: boolean } | null {
  if (view.activity === "installing") {
    return { text: t("windows.ui.thisonlytakesamoment"), live: false };
  }
  if (view.display === "checking" || view.activity === "checking") {
    if (view.downloaded_version) {
      return {
        text: t("windows.ui.solstonearg0installsthenexttimethesolstoneappsta", { "arg0": String(view.downloaded_version) }),
        live: false,
      };
    }
    return null;
  }
  if (view.display === "downloading" || view.activity === "downloading") {
    return null;
  }
  if (view.display === "staged" || view.installs_next_start) {
    let text = t("windows.ui.itinstallsthenexttimethesolstoneappstartsincludi");
    if (view.cost_deferral) {
      text += t("windows.ui.arg0", { "arg0": String(costDeferralText(view.cost_deferral)) });
    }
    if (view.recovery === "check") {
      text += t("windows.ui.thelastcheckforupdatesfailed");
    } else if (view.recovery === "download") {
      text += t("windows.ui.thedownloaddidnotfinish");
    } else if (view.recovery === "install") {
      text += t("windows.ui.theupdatecouldntbeinstalled");
    }
    return { text, live: false };
  }
  if (view.cost_deferral) {
    return { text: costDeferralText(view.cost_deferral), live: false };
  }
  switch (view.display) {
    case "never_checked":
      return {
        text: view.prefs.auto_check
          ? t("windows.ui.automaticchecksareon")
          : t("windows.ui.automaticchecksareoff"),
        live: false,
      };
    case "unavailable":
      return {
        text: t("windows.ui.downloadthelatestfromsolstoneappdownloadwindows"),
        live: false,
      };
    case "up_to_date":
    case "available":
    case "failed":
    case "failed_with_available":
      return { text: lastCheckedRelative(view.last_checked_at, secsNow), live: true };
  }
}

function actionButton(
  labelText: string,
  automationId: string | undefined,
  enabled: boolean,
  onClick: () => void,
  busy = false,
  variant: "standard" | "accent" = "accent",
): HTMLButtonElement {
  const effEnabled = enabled && !busy;
  const b = document.createElement("button");
  b.setAttribute("type", "button");
  b.type = "button";
  b.textContent = labelText;
  b.disabled = !effEnabled;
  if (busy) {
    b.setAttribute("aria-busy", "true");
  }
  if (automationId) {
    b.dataset.automationId = automationId;
  }
  const isAccent = variant === "accent";
  b.classList.add(effEnabled ? (isAccent ? "fluent-accent" : "fluent-control") : "fluent-control");
  b.style.fontSize = "13px";
  b.style.padding = "6px 12px";
  b.style.border = effEnabled ? (isAccent ? "1px solid var(--accent)" : "1px solid var(--border)") : "1px solid var(--border)";
  b.style.borderRadius = "var(--radius-control)";
  b.style.background = effEnabled ? (isAccent ? "var(--accent)" : "var(--fill)") : "var(--fill)";
  b.style.color = effEnabled ? (isAccent ? "var(--accent-fg)" : "var(--fg)") : "var(--muted)";
  b.style.cursor = effEnabled ? "pointer" : "default";
  if (effEnabled) {
    b.onclick = onClick;
  }
  return b;
}

function frequencyRow(interval: CheckIntervalKind, enabled: boolean): HTMLElement {
  const row = document.createElement("div");
  row.style.display = "flex";
  row.style.alignItems = "center";
  row.style.gap = "10px";
  row.style.padding = "6px 0";
  row.style.marginLeft = "26px";

  const labelNode = text("div", t("windows.desktop.shared.updates.howoften"));
  labelNode.style.fontSize = "13px";
  labelNode.style.color = enabled ? "var(--fg-subtle)" : "var(--muted)";

  const sel = document.createElement("select");
  sel.disabled = !enabled;
  sel.dataset.automationId = ids["settings.updates.frequency"];
  sel.setAttribute("aria-label", t("windows.ui.howoftentocheckforupdates"));
  sel.classList.add("fluent-control");
  const options: ReadonlyArray<readonly [CheckIntervalKind, string]> = [
    ["day", t("windows.desktop.shared.updates.everyday")],
    ["week", t("windows.desktop.shared.updates.everyweek")],
    ["month", t("windows.desktop.shared.updates.everymonth")],
  ];
  for (const [val, lbl] of options) {
    const opt = document.createElement("option");
    opt.value = val;
    opt.textContent = lbl;
    if (val === interval) {
      opt.selected = true;
    }
    sel.append(opt);
  }
  sel.style.fontSize = "13px";
  sel.style.padding = "3px 6px";
  sel.style.borderRadius = "var(--radius-control)";
  sel.onchange = () => {
    void invoke("update_set_interval", { interval: sel.value });
  };

  row.append(labelNode, sel);
  return row;
}

// A settings toggle row: checkbox adjacent to its label (the macOS Toggle idiom),
// distinct from valueRow's split label : control grid.
function toggleRow(
  labelText: string,
  automationId: string,
  checked: boolean,
  onChange: (on: boolean) => void,
  busy = false,
): HTMLElement {
  const lab = document.createElement("label");
  lab.style.display = "flex";
  lab.style.alignItems = "center";
  lab.style.gap = "9px";
  lab.style.padding = "6px 0";
  lab.style.fontSize = "13px";
  lab.style.color = "var(--fg)";
  lab.style.cursor = "pointer";

  const box = document.createElement("input");
  box.type = "checkbox";
  box.checked = checked;
  box.disabled = busy;
  if (busy) {
    box.setAttribute("aria-busy", "true");
  }
  box.dataset.automationId = automationId;
  box.style.margin = "0";
  box.onchange = () => onChange(box.checked);

  lab.append(box, text("span", labelText));
  return lab;
}

// A thin update-progress bar: determinate (download percent) or an indeterminate
// sweep while a check/install is in flight, so a wait visibly breathes.
function updateProgressBar(determinate: boolean, pct: number | null): HTMLElement {
  const wrap = document.createElement("div");
  wrap.style.display = "flex";
  wrap.style.alignItems = "center";
  wrap.style.gap = "10px";
  wrap.style.margin = "12px 0 2px";

  const track = document.createElement("div");
  track.style.position = "relative";
  track.style.flex = "1";
  track.style.height = "6px";
  track.style.borderRadius = "3px";
  track.style.background = "var(--fill)";
  track.style.overflow = "hidden";

  const fill = document.createElement("div");
  fill.style.position = "absolute";
  fill.style.top = "0";
  fill.style.bottom = "0";
  fill.style.background = "var(--accent)";
  fill.style.borderRadius = "3px";
  if (determinate) {
    fill.style.left = "0";
    fill.style.width = t("windows.ui.arg0x211b8ba1", { "arg0": String(pct ?? 0) });
    fill.style.transition = "width .2s";
  } else {
    fill.style.width = "35%";
    fill.style.animation = "update-indeterminate 1.1s ease-in-out infinite";
  }
  track.append(fill);
  wrap.append(track);

  if (determinate) {
    const lbl = text("div", t("windows.ui.arg0x211b8ba1", { "arg0": String(pct ?? 0) }));
    lbl.style.fontSize = "12px";
    lbl.style.color = "var(--fg-subtle)";
    lbl.style.minWidth = "34px";
    lbl.style.textAlign = "right";
    wrap.append(lbl);
  }
  return wrap;
}

// Release notes from the feed, rendered as light markdown (headings + bullets)
// in a quiet card. Carries the settings.updates.notes automation id.
function updateNotesBlock(notes: string): HTMLElement {
  const wrap = document.createElement("div");
  wrap.style.margin = "14px 0 2px";

  const cap = text("div", t("windows.desktop.shared.updates.whatsnew"));
  cap.style.fontSize = "12px";
  cap.style.fontWeight = "600";
  cap.style.color = "var(--fg-subtle)";
  cap.style.margin = "0 0 6px";

  const card = automation(document.createElement("div"), ids["settings.updates.notes"]);
  card.classList.add("scroll-surface");
  card.style.background = "var(--bg-input)";
  card.style.border = "1px solid var(--border-subtle)";
  card.style.borderRadius = "var(--radius-card)";
  card.style.padding = "10px 12px";
  card.style.maxHeight = "150px";
  card.style.overflowY = "auto";

  for (const raw of notes.split("\n")) {
    const line = raw.trim();
    if (!line) {
      const spacer = document.createElement("div");
      spacer.style.height = "5px";
      card.append(spacer);
      continue;
    }
    const heading = /^#{1,3}\s+(.*)$/.exec(line);
    const bullet = /^[-*]\s+(.*)$/.exec(line);
    if (heading) {
      const h = text("div", heading[1]);
      h.style.fontSize = "12.5px";
      h.style.fontWeight = "600";
      h.style.color = "var(--fg)";
      h.style.margin = "6px 0 3px";
      card.append(h);
    } else if (bullet) {
      const row = document.createElement("div");
      row.style.display = "flex";
      row.style.gap = "7px";
      row.style.fontSize = "12.5px";
      row.style.color = "var(--fg-subtle)";
      row.style.lineHeight = "1.45";
      row.style.margin = "2px 0";
      const dot = text("div", "•");
      dot.style.color = "var(--accent)";
      row.append(dot, text("div", bullet[1]));
      card.append(row);
    } else {
      const p = text("div", line);
      p.style.fontSize = "12.5px";
      p.style.color = "var(--fg-subtle)";
      p.style.lineHeight = "1.45";
      p.style.margin = "2px 0";
      card.append(p);
    }
  }
  wrap.append(cap, card, updateNotesOnlineLink());
  return wrap;
}

// The macOS "read the full notes online" affordance (UpdatesCopy.releaseNotesOnlineURL).
// Lives inside the notes block, so it renders only when the feed actually carried
// notes — the same NotesMarkdown the web page renders from the one releases.win.json
// feed — and can therefore never point at an empty release-history page. The webview
// is a sealed renderer with no navigation power: the href mirrors the destination for
// hover/a11y semantics but is never followed (a raw nav would replace the Settings
// shell); the click is handed to the backend, which opens the system browser.
function updateNotesOnlineLink(): HTMLAnchorElement {
  const link = document.createElement("a");
  link.textContent = t("windows.desktop.shared.updates.textcontent");
  link.href = "https://solstone.app/releases/windows";
  link.style.display = "inline-block";
  link.style.margin = "8px 0 0";
  link.style.fontSize = "12.5px";
  link.style.color = "var(--accent)";
  link.style.textDecoration = "none";
  link.style.cursor = "pointer";
  link.onmouseenter = () => {
    link.style.textDecoration = "underline";
  };
  link.onmouseleave = () => {
    link.style.textDecoration = "none";
  };
  link.onclick = (e) => {
    e.preventDefault();
    void invoke("open_release_notes");
  };
  return link;
}

function renderUpdatesSection(view: UpdateView): HTMLElement {
  const pane = section(t("windows.desktop.shared.settings.updates"));
  const a = view.actions;

  // Focal state headline + a quiet subtitle (the macOS Updates header hierarchy),
  // not another label : value row. The headline carries the contract state id.
  const headline = automation(text("div", updateHeadline(view)), ids["settings.updates.state"]);
  headline.style.fontSize = "16px";
  headline.style.fontWeight = "650";
  headline.style.color = "var(--fg)";
  headline.style.lineHeight = "1.3";
  headline.style.margin = "0";
  pane.append(headline);

  // The last-checked clock lives in the subtitle and ticks live (<60s "just
  // now") only where the subtitle *is* the clock; static subtitles (staged,
  // unavailable, never-checked) never tick, so lastCheckedEl stays null there.
  lastCheckedEl = null;
  const subtitle = updateSubtitle(view, nowSecs());
  if (subtitle) {
    const sub = text("div", subtitle.text);
    sub.style.fontSize = "13px";
    sub.style.color = "var(--fg-subtle)";
    sub.style.margin = "3px 0 0";
    if (subtitle.live) {
      automation(sub, ids["settings.updates.lastChecked"]);
      lastCheckedEl = sub;
    }
    pane.append(sub);
  }
  // Always render settings.updates.lastChecked regardless of state (including checking,
  // downloading, staged, and never-checked).
  if (lastCheckedEl == null) {
    const lastCheckedDiv = automation(
      text("div", lastCheckedRelative(view.last_checked_at, nowSecs())),
      ids["settings.updates.lastChecked"],
    );
    lastCheckedDiv.style.fontSize = "13px";
    lastCheckedDiv.style.color = "var(--fg-subtle)";
    lastCheckedDiv.style.margin = "3px 0 0";
    lastCheckedEl = lastCheckedDiv;
    pane.append(lastCheckedDiv);
  }

  // Progress — determinate for a download, an indeterminate sweep while a check
  // or install is in flight.
  if (view.display === "checking" || view.activity === "installing") {
    pane.append(updateProgressBar(false, null));
  } else if (view.display === "downloading") {
    pane.append(updateProgressBar(true, view.download_pct));
  }

  // Release notes — only present in the available state (the reducer clears them
  // once a version downloads); rendered as light markdown.
  if (view.display === "available" && view.notes) {
    pane.append(updateNotesBlock(view.notes));
  }

  // Action buttons — each shown only when relevant, each disabled from real
  // actionability (no dead buttons). There is no cancel control (Velopack 1.2.0
  // has no cancellation API); an in-flight download/install shows no buttons.
  const actions = document.createElement("div");
  actions.style.display = "flex";
  actions.style.flexWrap = "wrap";
  actions.style.gap = "8px";
  actions.style.margin = "14px 0 2px";
  let anyAction = false;

  const showCheck =
    view.display !== "downloading" &&
    view.display !== "unavailable" &&
    view.activity !== "installing";
  if (showCheck) {
    const checkLabel = view.display === "never_checked" ? t("windows.ui.checkforupdates") : t("windows.desktop.shared.copy.checkagain");
    actions.append(
      actionButton(checkLabel, ids["settings.updates.checkNow"], a.can_check_now, () => {
        void invoke("update_check_now");
      }),
    );
    anyAction = true;
  }
  if ((view.display === "available" || a.can_download) && view.recovery !== "download") {
    actions.append(
      actionButton(t("windows.desktop.shared.updates.download"), ids["settings.updates.download"], a.can_download, () => {
        void invoke("update_download");
      }),
    );
    anyAction = true;
  }
  if (view.installs_next_start && view.activity !== "installing") {
    actions.append(
      actionButton(t("windows.ui.relaunchtoinstall"), ids["settings.updates.install"], a.can_install, () => {
        void invoke("update_install");
      }),
    );
    anyAction = true;
  }
  if ((view.display === "failed" || view.display === "failed_with_available" || view.recovery === "download") && view.recovery !== "install") {
    const isDownloadRecovery = view.recovery === "download";
    actions.append(
      actionButton(t("windows.desktop.shared.settings.textcontent"), ids["settings.updates.retry"], isDownloadRecovery ? a.can_download : a.can_retry, () => {
        if (isDownloadRecovery) {
          void invoke("update_download");
        } else {
          void invoke("update_check_now");
        }
      }),
    );
    anyAction = true;
  }
  if (a.can_dismiss) {
    actions.append(
      actionButton(t("windows.desktop.shared.updates.dismiss"), ids["settings.updates.dismiss"], true, () => {
        void invoke("update_dismiss");
      }),
    );
    anyAction = true;
  }
  if (anyAction) {
    pane.append(actions);
  }

  // Automatic-update preferences, grouped (the macOS "automatic updates" box):
  // the auto-check toggle, the frequency picker indented beneath it (disabled
  // when auto-check is off), and the background-download toggle.
  const prefs = document.createElement("div");
  prefs.style.marginTop = "16px";
  prefs.style.paddingTop = "14px";
  prefs.style.borderTop = "1px solid var(--border-subtle)";

  const prefsLabel = text("div", t("windows.desktop.shared.updates.automaticupdates"));
  prefsLabel.style.fontSize = "12px";
  prefsLabel.style.fontWeight = "600";
  prefsLabel.style.color = "var(--fg-subtle)";
  prefsLabel.style.margin = "0 0 4px";
  prefs.append(prefsLabel);

  prefs.append(
    toggleRow(
      t("windows.desktop.shared.updates.checkforupdatesautomatically"),
      ids["settings.updates.autoCheck"],
      view.prefs.auto_check,
      (on) => {
        void invoke("update_set_auto_check", { on });
      },
    ),
  );
  prefs.append(frequencyRow(view.prefs.interval, a.frequency_enabled));
  prefs.append(
    toggleRow(
      t("windows.desktop.shared.updates.downloadupdatesinthebackground"),
      ids["settings.updates.autoDownload"],
      view.prefs.auto_download,
      (on) => {
        void invoke("update_set_auto_download", { on });
      },
    ),
  );
  pane.append(prefs);

  // Privacy footnote — the trust line for an update surface, ported from the
  // macOS pane. Honest by construction: the check is a first-party manifest GET
  // with no per-user identifier (the stagingId is neutralized — Article 8).
  const foot = document.createElement("div");
  foot.style.marginTop = "16px";
  foot.style.paddingTop = "12px";
  foot.style.borderTop = "1px solid var(--border-subtle)";
  const footText = text(
    "div",
    t("windows.ui.nousagedataiseversentupdate"),
  );
  footText.style.fontSize = "12px";
  footText.style.color = "var(--fg-subtle)";
  footText.style.lineHeight = "1.45";
  foot.append(footText);
  pane.append(foot);

  return pane;
}

function rerender(): void {
  const hadFocusWizardPrimary = focusWizardPrimary;
  const active = document.activeElement;
  let activeSelector: string | null = null;
  if (!hadFocusWizardPrimary && active && active !== document.body) {
    const autoId = active.getAttribute("data-automation-id");
    if (autoId) {
      activeSelector = `[data-automation-id="${autoId}"]`;
    } else if (active.id) {
      activeSelector = `#${active.id}`;
    }
  }

  // Any full render satisfies a deferred background request, so direct callers
  // (commits, hotkey-capture end, boot) auto-flush a pending coalesced rerender.
  pendingRerender = false;
  if (!latestHealth) {
    renderUnavailable();
  } else if (label === "about") {
    renderAbout(latestHealth);
  } else {
    renderSettings(latestHealth);
  }
  scheduleRenderBeacon();

  if (!hadFocusWizardPrimary && activeSelector) {
    const el = document.querySelector(activeSelector) as HTMLElement | null;
    if (el && typeof el.focus === "function") {
      el.focus();
    }
  }
}

// Background event streams call this instead of rerender() directly: defer + coalesce
// the full rerender while a control is active so an open native popup / in-progress
// edit / hotkey capture isn't torn down; otherwise render immediately (no added latency).
// A pairing link opened while this window is already open is stored by a
// second launch, which only brings this window forward: nothing in health
// changes, so no render would read it. Coming forward re-reads it into a
// field the owner hasn't touched.
function refreshPairingPrefillOnFocus(): void {
  if (label !== "settings" || latestHealth?.setup?.sequence_enabled !== true) return;
  if (latestHealth.sync.pairing.phase !== "not_paired") return;
  if (pairFieldDirty || pairingDraft) return;
  waitingLinkChecked = false;
  requestRerender();
}

function requestRerender(): void {
  if (isInteractiveControlActive()) {
    pendingRerender = true;
    return;
  }
  rerender();
}

function renderUnavailable(): void {
  const rootId = label === "about" ? ids["about.window.root"] : ids["settings.window.root"];
  resetRoot(rootId);
  syncSetupSun(false);

  const title = text("h1", "solstone");
  title.style.margin = "0";
  title.style.padding = "18px 20px 8px";
  title.style.fontSize = "22px";
  title.style.fontWeight = "700";

  const msg = text("p", t("windows.ui.couldntloadthesolstoneapps"));
  msg.style.padding = "0 20px";
  msg.style.color = "var(--fg-subtle)";

  const retry = document.createElement("button");
  retry.textContent = t("windows.desktop.shared.settings.textcontent");
  retry.classList.add("fluent-accent");
  retry.style.margin = "8px 20px";
  retry.style.fontSize = "13px";
  retry.style.padding = "7px 14px";
  retry.style.border = "1px solid var(--accent)";
  retry.style.borderRadius = "var(--radius-control)";
  retry.style.background = "var(--accent)";
  retry.style.color = "var(--accent-fg)";
  retry.style.cursor = "pointer";
  retry.addEventListener("click", () => {
    void retryHealth();
  });

  root.append(title, msg, retry);
}

async function retryHealth(): Promise<void> {
  try {
    const health = await invoke<HealthDump>("get_health");
    if (health) {
      setLatestHealth(health);
    }
  } catch {
    // Ignore error
  }
  requestRerender();
}

function scheduleRenderBeacon(): void {
  if (renderBeaconFired) {
    return;
  }
  const rootId = label === "about" ? ids["about.window.root"] : ids["settings.window.root"];
  requestAnimationFrame(() => {
    // Gate on OUR contract window-root being present — proves our renderer painted,
    // not a foreign error page. Fires for both the real UI and the error state
    // (both call resetRoot with the contract root id); never if our JS never runs.
    if (document.querySelector(`[data-automation-id="${rootId}"]`)) {
      renderBeaconFired = true;
      try {
        void invoke("view_rendered").catch(() => {});
      } catch {
        // Fire-and-forget only.
      }
    }
  });
}

let browserRefreshAt = 0;

// The browser row follows the health heartbeat, at most every few seconds.
async function refreshBrowserStatus(): Promise<void> {
  const now = Date.now();
  if (now - browserRefreshAt < 5000) {
    return;
  }
  browserRefreshAt = now;
  latestBrowser = await invoke<BrowserStatus | null>("browser_status").catch(() => null);
}

let obsidianFetching = false;

// The Obsidian row: fetched with the heartbeat, and every couple of seconds while
// the source is doing something and Settings is on screen. Renders once the
// answer is in, and only when it says something new.
async function refreshObsidianStatus(): Promise<void> {
  if (obsidianFetching) {
    return;
  }
  obsidianFetching = true;
  try {
    const doc = await invoke<ObsidianDocument | null>("obsidian_status").catch(() => null);
    const next = doc ? doc.status : null;
    const changed = JSON.stringify(next) !== JSON.stringify(latestObsidian);
    latestObsidian = next;
    if (next?.state === "off") {
      const listed = await invoke<ObsidianVault[] | null>("obsidian_found_vaults").catch(() => null);
      obsidianFound = listed ?? [];
    }
    if (changed) {
      requestRerender();
    }
  } finally {
    obsidianFetching = false;
  }
}

let screenshotsFetching = false;

// The screenshots page: fetched with the heartbeat, and every couple of seconds
// while the source is set up and Settings is on screen. A change also re-reads
// the counters, so `so far` moves as screenshots come in.
async function refreshScreenshotsStatus(): Promise<void> {
  if (screenshotsFetching) return;
  screenshotsFetching = true;
  try {
    const doc = await invoke<ScreenshotsDocument | null>("screenshots_status").catch(() => null);
    const next = doc ? doc.status : null;
    const changed = JSON.stringify(next) !== JSON.stringify(latestScreenshots);
    latestScreenshots = next;
    if (changed) {
      countersFor = null;
      requestRerender();
    }
  } finally {
    screenshotsFetching = false;
  }
}

function pollScreenshotsStatus(): Promise<void> {
  if (label !== "settings" || document.visibilityState === "hidden" || screenshotsBusy) {
    return Promise.resolve();
  }
  if (
    !latestScreenshots ||
    latestScreenshots.state === "off" ||
    latestScreenshots.state === "not_set_up" ||
    latestScreenshots.state === "no_folder"
  ) {
    return Promise.resolve();
  }
  return refreshScreenshotsStatus();
}

async function applyScreenshotsDocument(run: Promise<ScreenshotsDocument | null>): Promise<void> {
  screenshotsBusy = true;
  requestRerender();
  try {
    const document = await run;
    if (document) {
      latestScreenshots = document.status;
      countersFor = null;
    }
  } catch {
    // The page keeps what it showed; the next read says where things are.
  }
  screenshotsBusy = false;
  requestRerender();
}

function screenshotsCommand(command: string, args?: Record<string, unknown>): void {
  void applyScreenshotsDocument(
    args ? invoke<ScreenshotsDocument | null>(command, args) : invoke<ScreenshotsDocument | null>(command),
  );
}

/** The tile's line: the folder while on, progress while bringing in, the pause's own words. */
function screenshotsTileLine(status: ScreenshotsStatus | null): { companion: string; setup: string; attention: string } {
  const companion =
    status?.state === "bringing_in" && status.backlog && !status.waiting_for_journal
      ? SCREENSHOTS_COPY.S5(status.backlog.done, status.backlog.total)
      : status?.state === "paused"
        ? SCREENSHOTS_COPY.S16ComputerPause
        : (status?.state === "on" || status?.state === "bringing_in") && status.folder_line
          ? SCREENSHOTS_COPY.S4(status.folder_line)
          : "";
  return {
    companion,
    setup: status?.state === "choosing" ? SCREENSHOTS_COPY.S11 : "",
    attention: status?.state === "folder_missing" ? SCREENSHOTS_COPY.D9(status.folder_name ?? "Screenshots") : "",
  };
}

function screenshotsFact(labelText: string, value: HTMLElement | string, extra?: HTMLElement): HTMLElement {
  const row = document.createElement("div");
  row.className = "desk-fact";
  const label = text("span", labelText);
  label.className = "desk-fact-label";
  const mark = typeof value === "string" ? text("span", value) : value;
  mark.classList.add("desk-fact-value");
  row.append(label, mark);
  if (extra) row.append(extra);
  return row;
}

function screenshotsLine(sentence: string): HTMLElement {
  const node = text("p", sentence);
  node.className = "desk-fact-line";
  return node;
}

/** The screenshots page, in the desktop shell's page order. */
function renderScreenshotsPage(body: HTMLElement): HTMLElement {
  const status = latestScreenshots;
  if (!status) return body;
  const copy = SCREENSHOTS_COPY;
  const page = screenshotsPage({
    state: status.state,
    countFailed: status.count_failed,
    waitingForJournal: status.waiting_for_journal,
    windowsFolder: status.windows_folder,
    hasFolder: status.folder !== null,
    confirmed: status.state === "on" || status.state === "bringing_in" || status.state === "paused",
  });
  // The reason, and the state's one action.
  if (page.reason) {
    const folderName = status.folder_name ?? "Screenshots";
    const sentence =
      page.reason === "noFolder"
        ? copy.D8
        : page.reason === "folderGone"
          ? copy.D9(folderName)
          : page.reason === "countFailed"
            ? copy.S12
            : page.reason === "paused"
              ? copy.S16ComputerPause
              : page.reason === "waitingForJournal"
                ? copy.S21
                : copy.S6(status.backlog?.done ?? 0, status.backlog?.total ?? 0);
    body.append(automation(deskReason(sentence), ids["settings.sources.screenshots.reason"]));
  }
  if (page.action === "tryAgain") {
    const again = deskAction(copy.S12Action, () => screenshotsCommand("screenshots_try_again"), ids["settings.sources.screenshots.tryAgain"]);
    again.disabled = screenshotsBusy;
    body.append(again);
  } else if (page.action === "resume") {
    body.append(
      deskAction(copy.resume, () => {
        void invoke("resume").then(() => retryHealth());
      }),
    );
  }
  // What it takes in, and what stays where it is.
  body.append(breath(copy.D1));
  if (page.otherFilesStay && status.folder_name) body.append(breath(copy.D3(status.folder_name)));
  // How far back.
  if (page.chooser) {
    const chooser = document.createElement("fieldset");
    chooser.className = "desk-choices";
    automation(chooser, ids["settings.sources.screenshots.span"]);
    const legend = document.createElement("legend");
    legend.textContent = copy.S7;
    chooser.append(legend);
    for (const span of ["thirty_days", "six_months", "year", "all", "nothing"] as const) {
      const choice = document.createElement("label");
      choice.className = "desk-choice";
      const input = document.createElement("input");
      input.type = "radio";
      input.name = "screenshots-span";
      input.value = span;
      input.checked = screenshotsSpan === span;
      input.onchange = () => {
        screenshotsSpan = span;
        requestRerender();
      };
      choice.append(input, text("span", copy.S8[span]));
      chooser.append(choice);
    }
    body.append(chooser, breath(copy.S9));
    if (status.turned_back_on) body.append(breath(copy.S17));
    const confirm = screenshotsConfirm({
      span: screenshotsSpan,
      count: status.counts?.[screenshotsSpan] ?? null,
      counting: status.counting || status.count_failed,
    });
    const labelText =
      confirm.kind === "checking" ? copy.checking : confirm.kind === "today" ? copy.S10Today : copy.S10(confirm.count);
    const button = deskAction(labelText, () => screenshotsCommand("screenshots_confirm", { span: screenshotsSpan }), ids["settings.sources.screenshots.confirm"]);
    button.disabled = confirm.kind === "checking" || screenshotsBusy;
    body.append(button);
  }
  // So far, last taken in, and the facts.
  const totals = deskFactBlock("screenshots");
  if (totals) body.append(totals);
  const facts = document.createElement("div");
  facts.className = "desk-facts";
  facts.append(screenshotsLine(copy.S14), screenshotsLine(copy.S15));
  facts.append(screenshotsLine(page.everyPicture ? copy.D16(status.folder_name ?? "Screenshots") : copy.S23));
  const choose = deskAction(copy.chooseFolder, () => screenshotsCommand("screenshots_choose_folder"), ids["settings.sources.screenshots.chooseFolder"]);
  choose.disabled = screenshotsBusy;
  const folder = selectable(text("span", status.folder ?? ""));
  facts.append(automation(screenshotsFact(copy.S22, folder, choose), ids["settings.sources.screenshots.folder"]));
  if (status.last_checked_ms !== null) {
    facts.append(screenshotsFact(copy.S18, lastSyncedLabel(status.last_checked_ms, nowSecs())));
  }
  if (status.waiting > 0) facts.append(screenshotsFact(copy.S19, String(status.waiting)));
  body.append(facts);
  // What can't come in, and what couldn't be read.
  if (page.finalLine && status.cant_come_in > 0) body.append(deskReason(copy.D12(status.cant_come_in)));
  if (status.unreadable > 0) {
    body.append(deskReason(copy.D13(status.unreadable)));
    const again = deskAction(copy.D13Action, () => screenshotsCommand("screenshots_try_again"));
    again.disabled = screenshotsBusy;
    body.append(again);
  }
  const more = document.createElement("details");
  more.className = "desk-more";
  const summary = document.createElement("summary");
  summary.textContent = copy.D15;
  more.append(summary, screenshotsLine(copy.D17));
  body.append(more);
  return body;
}

function pollObsidianStatus(): Promise<void> {
  if (label !== "settings" || document.visibilityState === "hidden" || obsidianBusy) {
    return Promise.resolve();
  }
  if (!latestObsidian || latestObsidian.state === "off") {
    return Promise.resolve();
  }
  return refreshObsidianStatus();
}

async function boot(): Promise<void> {
  if (label === "about") {
    setLatestHealth(await invoke<HealthDump>("get_health").catch(() => null));
    rerender();
    return;
  }
  const [health, storage, update, exclusions, apps, hotkey, micCfg, mics, migration] = await Promise.all([
    invoke<HealthDump>("get_health").catch(() => null),
    invoke<StorageInfo>("storage_info").catch(() => null),
    invoke<UpdateView>("update_get").catch(() => null),
    invoke<ExclusionRules>("get_exclusions").catch(() => null),
    invoke<RunningApp[]>("list_running_apps").catch(() => [] as RunningApp[]),
    invoke<HotkeyView>("get_hotkey").catch(() => null),
    invoke<MicView>("get_mic_config").catch(() => null),
    invoke<MicDeviceRef[]>("list_mic_devices").catch(() => [] as MicDeviceRef[]),
    invoke<MigrationSnapshot>("pairing_migration_state").catch(
      () => ({ migration: null, issue: "storage_unavailable" }) as MigrationSnapshot,
    ),
  ]);
  setLatestHealth(health);
  latestStorage = storage;
  latestUpdate = update;
  latestExclusions = exclusions;
  runningApps = apps;
  latestHotkey = hotkey;
  latestMic = micCfg;
  micDevices = mics;
  latestMigration = migration ?? { migration: null, issue: null };
  migrationOfferBinding = latestMigration.migration?.offer_binding ?? null;
  migrationRefreshAt = Date.now();
  await refreshBrowserStatus();
  await refreshObsidianStatus();
  await refreshScreenshotsStatus();
  await refreshImportStatus();
  rerender();
}

function updatePauseLabels(): void {
  if (pauseStatusEls.length === 0) return;
  const secs = latestHealth?.pause?.seconds_remaining;
  if (latestHealth?.app_state !== "paused" || secs == null || healthReceivedAt == null) {
    return;
  }
  const remaining = Math.max(0, secs - (nowSecs() - healthReceivedAt));
  const labelText = pausedStatusText(remaining);
  for (const el of pauseStatusEls) {
    el.textContent = labelText;
  }
}

export function start(): void {
  applyDocumentLocale();
  installFrontendErrorHandlers();

  const queriedRoot = document.querySelector<HTMLDivElement>("#app");
  if (!queriedRoot) {
    throw new Error("missing app root");
  }
  root = queriedRoot;

  const queriedRoute = new URLSearchParams(window.location.search).get("route");
  if (queriedRoute && ["sources", "audio", "screen", "browsers", "obsidian"].includes(queriedRoute)) {
    startupDestination = queriedRoute;
    focusPaneTitleOnRender = true;
  }

  const styleEl = document.createElement("style");
  styleEl.textContent = NATIVE_FEEL_CSS + MARK_CSS;
  document.head.append(styleEl);

  const setupStyleEl = document.createElement("style");
  setupStyleEl.textContent = setupFrameCss;
  document.head.append(setupStyleEl);

  document.addEventListener("contextmenu", (event) => {
    if (isTextEntryTarget(event.target)) {
      return;
    }
    event.preventDefault();
  });

  document.addEventListener("keydown", (event) => {
    const key = event.key;
    const lowerKey = key.toLowerCase();
    const ctrlOrMeta = event.ctrlKey || event.metaKey;
    const zoomKey = key === "+" || key === "=" || key === "-" || key === "_" || key === "0";
    const blocked =
      key === "F5" ||
      key === "F3" ||
      key === "F7" ||
      (ctrlOrMeta &&
        (lowerKey === "r" ||
          lowerKey === "p" ||
          lowerKey === "f" ||
          zoomKey));

    if (blocked) {
      event.preventDefault();
    }
  });

  document.addEventListener(
    "wheel",
    (event) => {
      if (event.ctrlKey) {
        event.preventDefault();
      }
    },
    { passive: false },
  );

  // Flush a deferred background rerender once interaction ends. focusout fires before
  // the new focus settles, so recheck on a microtask (the blur->focusout->focus->focusin
  // sequence dispatches synchronously, so document.activeElement is settled by then) —
  // focus moving control->control doesn't flush; only settling onto a non-control does.
  document.addEventListener(
    "focusout",
    () => {
      queueMicrotask(() => {
        if (pendingRerender && !isInteractiveControlActive()) {
          rerender();
        }
      });
    },
    true,
  );

  narrowNav = window.matchMedia("(max-width: 719px)");

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && latestMigration?.migration?.offer_available) {
      migrationPromptHidden = true;
      migrationFlow = null;
      selectedMigrationDevice = null;
      requestRerender();
      return;
    }
    if (event.key !== "Escape" || !narrowNav.matches) {
      return;
    }
    const shell = settingsShell();
    if (shell && navOverlayOpen(shell)) {
      closeNavOverlay(true);
    }
  });

  document.addEventListener("click", (event) => {
    if (!narrowNav.matches) {
      return;
    }
    const shell = settingsShell();
    if (!shell || !navOverlayOpen(shell)) {
      return;
    }
    const target = event.target;
    if (!(target instanceof Node)) {
      return;
    }
    const element = target instanceof Element ? target : target.parentNode;
    if (!(element instanceof Element)) {
      return;
    }
    if (element.closest(".settings-rail") || element.closest(".settings-hamburger")) {
      return;
    }
    closeNavOverlay(true);
  });

  label = getCurrentWindow().label;

  void boot();
  void listen<string>("settings://navigate", (event) => {
    if (latestHealth && showsDesktopShell(latestHealth)) {
      const next = settingsNavigate(event.payload);
      deskRow = next.row;
      deskPane = next.pane;
      deskPhonePane = next.row !== "home" || next.pane !== "status";
      rerender();
      return;
    }
    // Before the first health the destination waits for it. Otherwise this is setup,
    // and focusing the window was the whole of opening it.
    if (!latestHealth) startupDestination = event.payload;
  });
  void listen<HealthDump>("health://changed", (event) => {
    setLatestHealth(event.payload);
    void refreshBrowserStatus();
    void refreshObsidianStatus();
    void refreshScreenshotsStatus();
    void refreshImportStatus();
    void refreshMigrationState();
    requestRerender();
  });
  void listen<ImportDocument>("import://changed", (event) => {
    latestImport = event.payload;
    requestRerender();
  });
  void listen<UpdateView>("update://changed", (event) => {
    latestUpdate = event.payload;
    requestRerender();
  });
  window.addEventListener("blur", () => setProofWindowActive(false));
  window.addEventListener("focus", () => {
    setProofWindowActive(true);
    refreshPairingPrefillOnFocus();
  });
  document.addEventListener("visibilitychange", () => setProofWindowActive(document.visibilityState !== "hidden"));
  window.addEventListener("pagehide", () => setProofWindowActive(false));
  void listen("proof://inactive", handleProofInactive);
  void listen("tauri://focus", () => {
    setProofWindowActive(true);
    refreshPairingPrefillOnFocus();
  });
  void listen<PreviewSnapshot>("preview://changed", (event) => {
    applyPreview(event.payload ?? {});
  });

  // Live last-checked clock: tick the relative string once a second without a full
  // re-render (the JS analog of the macOS TimelineView; <60s -> "just now").
  setInterval(() => {
    if (lastCheckedEl && latestUpdate) {
      lastCheckedEl.textContent = lastCheckedRelative(latestUpdate.last_checked_at, nowSecs());
    }
    updatePauseLabels();
    updateStatusClocks();
    lookForFoundJournal();
    lapseFoundJournalPress();
  }, 1000);
  setInterval(() => {
    void pollObsidianStatus();
    void pollScreenshotsStatus();
  }, 2500);
}

export const __test__ = {
  refreshPairingPrefillOnFocus,
  lookForFoundJournal,
  lapseFoundJournalPress,
  foundJournalPending: () => foundJournalLookInFlight,
  getHealth: () => latestHealth,
  updateStatusClocks,
  setRoot(el: HTMLDivElement) {
    root = el;
  },
  setLabel(l: string) {
    label = l;
  },
  setRoute(r: Route) {
    activeRoute = r;
  },
  getRoute(): Route {
    return activeRoute;
  },
  openDeskSource,
  getDesk() {
    return { row: deskRow, pane: deskPane };
  },
  setHealth(dump: HealthDump | null, receivedAt?: number | null) {
    latestHealth = dump;
    healthReceivedAt = receivedAt !== undefined ? receivedAt : (dump ? nowSecs() : null);
  },
  setClipboardWriter(writer: ((value: string) => Promise<void>) | null) {
    aboutClipboardWriter = writer;
  },
  setStorage(v: StorageInfo | null) {
    latestStorage = v;
  },
  setUpdate(v: UpdateView | null) {
    latestUpdate = v;
  },
  setExclusions(v: ExclusionRules | null) {
    latestExclusions = v;
  },
  setHotkey(v: HotkeyView | null) {
    latestHotkey = v;
  },
  setMic(v: MicView | null) {
    latestMic = v;
  },
  setMicDevices(v: MicDeviceRef[]) {
    micDevices = v;
  },
  setMigration(v: MigrationSnapshot | null) {
    latestMigration = v;
    migrationOfferBinding = v?.migration?.offer_binding ?? null;
  },
  setMigrationDevices(v: MigrationDevice[] | null) {
    latestMigrationDevices = v;
    migrationListUnavailable = v === null;
  },
  setMigrationFlow(v: MigrationFlow) {
    migrationFlow = v;
  },
  setRunningApps(v: RunningApp[]) {
    runningApps = v;
  },
  setBrowser(status: BrowserStatus | null) {
    latestBrowser = status;
  },
  pollObsidianStatus,
  pollScreenshotsStatus,
  setScreenshots(status: ScreenshotsStatus | null) {
    latestScreenshots = status;
    screenshotsBusy = false;
    screenshotsSpan = "thirty_days";
  },
  setCounters(doc: CounterDocument | null) {
    latestCounters = doc;
    countersFor = latestHealth;
  },
  setObsidian(status: ObsidianStatus | null, found: ObsidianVault[] = [], refusal: ObsidianRefusal | null = null) {
    latestObsidian = status;
    obsidianFound = found;
    obsidianRefusal = refusal;
    obsidianBusy = false;
    obsidianFailed = false;
  },
  setPreview(v: PreviewSnapshot | null) {
    applyPreview(v ?? {});
  },
  applyPreview,
  setAcceptedProofGeneration,
  fillProofNode,
  setProofWindowActive,
  handleProofInactive,
  navigateTo,
  proofIdle: () => proofOperations,
  renderSettings,
  renderAbout,
  renderUnavailable,
  renderWizard,
  rerender,
  reset() {
    syncSetupSun(false);
    try {
      localStorage.clear();
    } catch {
      // jsdom always provides localStorage; a real failure isn't testable state.
    }
    openProofKind = "closed";
    currentAcceptedProofGeneration = 0;
    proofRequest += 1;
    proofWindowActive = true;
    proofOperations = Promise.resolve();
    focusWizardPrimary = false;
    latestPreview = {};
    latestHealth = null;
    aboutClipboardWriter = null;
    healthReceivedAt = null;
    latestStorage = null;
    latestUpdate = null;
    latestMigration = null;
    latestMigrationDevices = null;
    migrationFlow = null;
    migrationBusy = false;
    migrationDevicesBusy = false;
    migrationListUnavailable = false;
    migrationShowDetails = false;
    migrationPromptHidden = false;
    migrationRefreshAt = 0;
    migrationRefreshInFlight = false;
    migrationOfferBinding = null;
    selectedMigrationDevice = null;
    latestExclusions = null;
    latestHotkey = null;
    exclusionsPending = null;
    exclusionsPersisted = true;
    latestMic = null;
    micDevices = [];
    runningApps = [];
    latestBrowser = null;
    latestObsidian = null;
    latestScreenshots = null;
    screenshotsBusy = false;
    screenshotsSpan = "thirty_days";
    obsidianFound = [];
    obsidianRefusal = null;
    obsidianBusy = false;
    obsidianFailed = false;
    browserDiscard = "idle";
    browserStoreFailure = null;
    activeRoute = "home";
    label = "settings";
    renderBeaconFired = false;
    focusPaneTitleOnRender = false;
    markCardFocused = false;
    pendingRerender = false;
    lastCheckedEl = null;
    pauseStatusEls = [];
    nextSaveEl = null;
    lastSyncedEl = null;
    pairingDraft = "";
    pairFieldDirty = false;
    waitingLinkChecked = false;
    pairingBusy = false;
    finishInFlight = false;
    browserInFlight = false;
    retryInFlight = false;
    nativeConnectInFlight = false;
    deferredInFlight = false;
    foundJournal = null;
    foundJournalLookInFlight = false;
    foundJournalLookedAt = 0;
    foundJournalConnecting = false;
    foundJournalPresses = 0;
    foundJournalConnected = false;
    foundJournalConnectedAt = null;
    pairingStatusEl = null;
    setupActionError.finish = "";
    setupActionError.browser = "";
    setupActionError.retry = "";
    setupActionError.connect = "";
    setupActionError.deferred = "";
    wizardPauseFailure = null;
    unknownLoginAttempt = null;
    hotkeyCapturing = false;
    titleDraft = "";
    deskRow = "home";
    deskPane = "status";
    deskShelfOpen = false;
    deskPhonePane = false;
    latestCounters = undefined;
    countersFor = null;
    startupDestination = null;
  },
};

export { applyPreview, setAcceptedProofGeneration, fillProofNode };
