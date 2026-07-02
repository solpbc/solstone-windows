#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Open a winget version-update PR (solpbc.Solstone -> microsoft/winget-pkgs) for a
# published release. Runs on the RELEASE HOST after `make publish` -- the GitHub
# release and its solstone-setup-<version>.exe asset must already exist.
#
# Operator-driven, no CI path. winget requires a PR per version (the community repo
# has no push API); this codifies the one command that opens it. The PR is validated
# by winget's pipeline (schema + hash + an interactive-sandbox install) before a
# moderator/bot merges -- so submitting is itself the install-validation gate.
#
# Requires komac (https://github.com/russellbanks/Komac), which fetches the asset,
# computes the SHA256, and opens the PR. NOTE: the FIRST version of a NEW package is
# a one-time `komac new` (done by hand); steady-state per-release is `komac update`,
# below. VERSION defaults to the workspace package version; pass an arg to override.
set -eu

. "$(dirname "$0")/lib/artifact-names.sh"

REPO="solpbc/solstone-windows"
PKG="solpbc.Solstone"

VERSION="${1:-$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')}"
[ -n "$VERSION" ] || { echo "publish-winget: could not determine VERSION (pass it as an arg)" >&2; exit 1; }
URL="$(winget_installer_url "$REPO" "$VERSION")"

if ! command -v komac >/dev/null 2>&1; then
  echo "publish-winget: komac not found." >&2
  echo "  Install it: cargo install komac  (or a release binary from" >&2
  echo "  https://github.com/russellbanks/Komac/releases), ensure a GitHub token is" >&2
  echo "  available (GITHUB_TOKEN or 'komac token update'), then re-run." >&2
  echo "  Manual fallback: bump packaging/winget/ (PackageVersion / InstallerUrl /" >&2
  echo "  InstallerSha256 / AppsAndFeaturesEntries.DisplayVersion) and open the PR by hand." >&2
  exit 1
fi

echo "publish-winget: komac update $PKG -> $VERSION ($URL)"
komac update "$PKG" --version "$VERSION" --urls "$URL" --submit
echo "publish-winget: PR submitted -- winget CI validates (interactive-sandbox install), then a moderator merges."
