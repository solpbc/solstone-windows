// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// The webview is a pure renderer. It subscribes to `health://changed` and paints
// the honest state it receives; it has no other input and cannot mint status.
// AutomationIds are stamped from the generated contract (see ./lib/contract.ts).

import { automationContract } from "./lib/contract";

const root = document.querySelector<HTMLDivElement>("#app");
if (root) {
  // Placeholder render. The Wave-1 shell work wires the health subscription and
  // the Status / Sources panes, stamping data-automation-id from the contract.
  root.textContent = "solstone — observers + journal";
  void automationContract;
}
