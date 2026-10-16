// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

import { invoke } from "@tauri-apps/api/core";
import { automationContract } from "./lib/contract";

export interface LogPreview {
  id: number;
  document: string;
  state: "ready" | "partial" | "empty" | "missing" | "unreadable";
  entries: number;
}

// The same node survives health renders. Reading is an explicit action; a
// background health change never replaces the document under review.
export function createLogHelp(): HTMLElement {
  const ids = automationContract.automation_ids;
  const pane = document.createElement("section");
  pane.className = "log-help";
  const heading = document.createElement("h2");
  heading.textContent = "recent logs";
  heading.className = "settings-card-title";
  const caption = document.createElement("p");
  caption.textContent = "read the solstone app's recent logs before saving a copy for a support report. nothing is sent from here.";
  const read = document.createElement("button");
  read.type = "button";
  read.className = "fluent-control";
  read.textContent = "read recent logs";
  read.dataset.automationId = ids["settings.logs.read"];
  const readStatus = document.createElement("p");
  readStatus.setAttribute("role", "status");
  const body = document.createElement("textarea");
  body.readOnly = true;
  body.spellcheck = false;
  body.wrap = "off";
  body.hidden = true;
  body.setAttribute("aria-label", "recent logs");
  body.dataset.automationId = ids["settings.logs.preview"];
  body.style.cssText = "width:100%;height:360px;box-sizing:border-box;font:12px Consolas,monospace;white-space:pre;resize:vertical;";
  const save = document.createElement("button");
  save.type = "button";
  save.className = "fluent-control";
  save.textContent = "save a copy…";
  save.hidden = true;
  save.disabled = true;
  save.dataset.automationId = ids["settings.logs.save"];
  const saveStatus = document.createElement("p");
  saveStatus.setAttribute("role", "status");
  let preview: LogPreview | null = null;

  read.onclick = async () => {
    preview = null;
    body.value = "";
    body.hidden = true;
    save.hidden = true;
    save.disabled = true;
    saveStatus.textContent = "";
    read.disabled = true;
    readStatus.textContent = "reading logs…";
    try {
      const next = await invoke<LogPreview>("read_recent_logs");
      const messages = {
        ready: "read the logs below, then choose whether to save a copy.",
        partial: "available logs are shown below. check the notes for gaps before saving a copy.",
        empty: "no recent logs are available yet. try again after using the app.",
        missing: "no log files are available yet. try again after using the app.",
        unreadable: "couldn't read the log files. try again.",
      };
      readStatus.textContent = messages[next.state];
      if (next.entries > 0) {
        preview = next;
        body.value = next.document;
        body.hidden = false;
        save.hidden = false;
        save.disabled = false;
      }
    } catch {
      readStatus.textContent = "couldn't read the logs. try again.";
    } finally {
      read.disabled = false;
    }
  };

  save.onclick = async () => {
    if (!preview) return;
    read.disabled = true;
    save.disabled = true;
    saveStatus.textContent = "";
    try {
      const saved = await invoke<boolean>("save_recent_logs", { previewId: preview.id });
      saveStatus.textContent = saved ? "copy saved." : "";
    } catch {
      saveStatus.textContent = "couldn't save the copy. try saving to another folder.";
    } finally {
      read.disabled = false;
      save.disabled = false;
      save.focus();
    }
  };
  pane.append(heading, caption, read, readStatus, body, save, saveStatus);
  return pane;
}
