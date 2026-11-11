// The desktop shell's document. Decisions live in derive.ts. This only paints.

export interface DeskTile {
  id: string;
  name: string;
  word: string;
  sub: string;
  switchOn: boolean | null;
  switchId: string | null;
  switchLabel: string;
  selected: boolean;
}

export interface DeskStatusRow {
  id: string;
  name: string;
  nothingYet: boolean;
  /** Empty when the counter read has no rows. That is not `nothing yet`. */
  total: string;
  relative: string;
  hasFacts: boolean;
}

export type DeskWidth = "full" | "narrower" | "phone";
export type DeskRow = "home" | "journal" | "thisPc" | "updates" | "help";

export interface DeskView {
  width: DeskWidth;
  row: DeskRow;
  shelfOpen: boolean;
  phoneShowsPane: boolean;
  greeting: string;
  pill: string;
  pausedLine: string | null;
  badges: readonly string[];
  tiles: readonly DeskTile[];
  rows: readonly DeskStatusRow[];
  paneTitle: string;
  paneIsStatus: boolean;
  /** Shelf pages replace the deck. Home keeps the deck beside the pane. */
  shelfPage: boolean;
}

export interface DeskParts {
  notice: HTMLElement | null;
  unread: HTMLElement | null;
  saveRetry: HTMLElement | null;
  paneExtra: HTMLElement | null;
  shelfBody: HTMLElement | null;
}

export interface DeskActions {
  onShelf: (row: DeskRow) => void;
  onShelfToggle: () => void;
  onBackHome: () => void;
  onPill: () => void;
  onResume: () => void;
  onClosePane: () => void;
  onTile: (id: string) => void;
  onSwitch: (id: string, on: boolean) => void;
  onStatus: (id: string) => void;
}

const SHELF: readonly { row: DeskRow; label: string }[] = [
  { row: "home", label: "home" },
  { row: "journal", label: "your journal" },
  { row: "thisPc", label: "this PC" },
  { row: "updates", label: "updates" },
  { row: "help", label: "help" },
];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function renderDesk(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const shell = el("div", "desk-shell");
  shell.dataset.width = view.width;
  shell.dataset.row = view.row;
  shell.dataset.shelf = view.shelfOpen ? "open" : "closed";
  shell.dataset.page = view.phoneShowsPane ? "pane" : "deck";

  shell.append(renderShelf(view, actions));

  if (view.shelfPage && parts.shelfBody) {
    const page = el("div", "desk-deck");
    page.setAttribute("data-automation-id", "settings.deck");
    page.append(parts.shelfBody);
    shell.append(page);
    return shell;
  }

  shell.append(renderDeck(view, parts, actions));
  shell.append(renderPane(view, parts, actions));
  return shell;
}

function renderShelf(view: DeskView, actions: DeskActions): HTMLElement {
  const rail = el("nav", "settings-rail");
  rail.id = "settings-nav";
  rail.setAttribute("aria-label", "settings");
  for (const item of SHELF) {
    const button = el("button", "settings-nav-item", item.label);
    button.type = "button";
    if (item.row === view.row) button.setAttribute("aria-current", "page");
    if (view.badges.includes(item.row)) {
      const dot = el("span", "desk-badge");
      dot.setAttribute("aria-hidden", "true");
      button.append(dot);
    }
    button.onclick = () => actions.onShelf(item.row);
    rail.append(button);
  }
  return rail;
}

function renderDeck(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const deck = el("div", "desk-deck");
  deck.setAttribute("data-automation-id", "settings.deck");
  if (view.width !== "full") {
    const opener = el("button", "desk-shelf-button", "home");
    opener.type = "button";
    opener.setAttribute("aria-label", "home");
    opener.onclick = () => actions.onShelfToggle();
    deck.append(opener);
  }
  if (parts.unread) deck.append(parts.unread);

  const head = el("div", "desk-head");
  const greeting = el("h1", "desk-greeting", view.greeting);
  greeting.tabIndex = -1;
  head.append(greeting);

  const pill = el("button", "desk-pill", view.pill);
  pill.type = "button";
  pill.onclick = () => actions.onPill();
  head.append(pill);
  deck.append(head);

  if (view.pausedLine) {
    deck.append(el("p", "desk-paused", view.pausedLine));
    const resume = el("button", "desk-resume", "resume");
    resume.type = "button";
    resume.onclick = () => actions.onResume();
    deck.append(resume);
  }

  if (parts.notice) deck.append(parts.notice);
  if (parts.saveRetry) deck.append(parts.saveRetry);

  const grid = el("div", "desk-grid");
  for (const tile of view.tiles) {
    grid.append(renderTile(tile, actions));
  }
  deck.append(grid);
  return deck;
}

function renderTile(tile: DeskTile, actions: DeskActions): HTMLElement {
  const card = el("div", "desk-tile");
  card.dataset.tile = tile.id;
  if (tile.selected) card.setAttribute("aria-current", "true");
  const open = el("button", "desk-tile-open");
  open.type = "button";
  open.append(el("span", "desk-tile-name", tile.name));
  if (tile.word) open.append(el("span", "desk-tile-word", tile.word));
  if (tile.sub) open.append(el("span", "desk-tile-sub", tile.sub));
  open.onclick = () => actions.onTile(tile.id);
  card.append(open);
  if (tile.switchOn !== null) {
    const label = el("label", "desk-tile-switch");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = tile.switchOn;
    if (tile.switchId) input.setAttribute("data-automation-id", tile.switchId);
    input.setAttribute("aria-label", tile.switchLabel);
    input.onchange = () => actions.onSwitch(tile.id, input.checked);
    label.append(input);
    card.append(label);
  }
  return card;
}

function renderPane(view: DeskView, parts: DeskParts, actions: DeskActions): HTMLElement {
  const pane = el("aside", "desk-pane");
  const head = el("div", "desk-pane-head");
  if (view.width === "phone") {
    const back = el("button", "desk-back", "‹ home");
    back.type = "button";
    back.setAttribute("aria-label", "home");
    back.onclick = () => actions.onBackHome();
    head.append(back);
  }
  const title = el("h2", "desk-pane-title", view.paneTitle);
  title.tabIndex = -1;
  head.append(title);
  if (!view.paneIsStatus) {
    const close = el("button", "desk-close", "×");
    close.type = "button";
    close.setAttribute("aria-label", "status");
    close.onclick = () => actions.onClosePane();
    head.append(close);
  }
  pane.append(head);

  if (view.paneIsStatus) {
    for (const row of view.rows) {
      pane.append(renderStatusRow(row, actions));
    }
  }
  if (parts.paneExtra) pane.append(parts.paneExtra);
  return pane;
}

function renderStatusRow(row: DeskStatusRow, actions: DeskActions): HTMLElement {
  const button = el("button", "desk-status-row");
  button.type = "button";
  if (!row.hasFacts || row.nothingYet) button.classList.add("is-empty");
  button.append(el("span", "desk-status-name", row.name));
  if (!row.hasFacts) {
    // A missing read is not a fact. The row still opens the page.
  } else if (row.nothingYet) {
    button.append(el("span", "desk-status-empty", "nothing yet"));
  } else {
    button.append(el("span", "desk-status-total", row.total));
    button.append(el("span", "desk-status-relative", row.relative));
  }
  button.onclick = () => actions.onStatus(row.id);
  return button;
}
