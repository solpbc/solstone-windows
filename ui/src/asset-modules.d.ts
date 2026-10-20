// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

declare module "*.svg?raw" {
  const source: string;
  export default source;
}
declare module "*.css?inline" {
  const css: string;
  export default css;
}
