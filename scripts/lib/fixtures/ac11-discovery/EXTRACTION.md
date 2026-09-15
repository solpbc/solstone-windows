# AC11 dual-platform discovery baseline

- Exact source commit: `3ad55472b047d8eb906db8c379f7a7012a867d79`
- Local index and worktree were clean before the first discovery command.
- Cargo.lock SHA-256: `5e96d042fe93e17c774033137961ed6163ed6402b24902b8388ff54e8dd3b4b8`
- ui/package-lock.json SHA-256: `544c4e8b8c49456a035b59a059a1b8a9dd5414db3224b266f8a6118c723248`

## Hosts and collection metadata

```text
timestamp=2026-09-15T06:02:21-06:00
hostname=suze
rustc 1.96.0 (ac68faa20 2026-05-25)
cargo 1.96.0 (30a34c682 2026-05-25)
windows_timestamp=Tue 09/15/2026  5:02:23.15
windows_host=SOL-WINBUILD
rustc 1.96.0 (ac68faa20 2026-05-25)
cargo 1.96.0 (30a34c682 2026-05-25)
```

Linux discovery ran in this worktree. Windows discovery ran as
`solbuild@sol-winbuild.local` in `%USERPROFILE%\\swbuild`, after the exact
source bundle had been sent and the remote checkout was confirmed at the exact
commit.

## Exact transfer and checkout commands

```sh
hop check --allow-capture -n 200 -- env WIN_REMOTE_HOST=solbuild@sol-winbuild.local EXPECTED_RELEASE_COMMIT=3ad55472b047d8eb906db8c379f7a7012a867d79 sh scripts/sync-win-host.sh
```

The transfer created a verified `refs/heads/__swsync` bundle and copied
`swbuild.bundle` plus `win-host-ci-source-binding.json` to the Windows host.
The checkout command used on the Windows host was:

```cmd
cd /d %USERPROFILE%\\swbuild
git fetch --force %USERPROFILE%\\swbuild.bundle refs/heads/__swsync:refs/heads/incoming
git checkout -f --detach incoming
git rev-parse HEAD
git diff --cached --quiet
git diff --quiet
```

The only Cargo commands executed on either host for this baseline were:

```text
cargo test --locked -p observer-pl -- --list
cargo test --locked -p pl-transport-win -- --list
```

Each `linux/` and `windows/` file contains only the actual `: test` entries
for one Cargo test target, sorted bytewise. Each `union/` file is the sorted,
deduplicated union of the corresponding Linux and Windows files. This preserves
Windows-only discovery, including DPAPI tests.

## union.digest construction

`union.digest` is the SHA-256 of the byte concatenation of these files, in
lexicographic byte order, with no separators beyond the newline each file
already ends with:

```text
union/observer-pl.ingest_v3.txt
union/observer-pl.lib.txt
union/observer-pl.observer_contract_conformance.txt
union/observer-pl.spl_pair_link_conformance.txt
union/pl-transport-win.integration_mode.txt
union/pl-transport-win.integration_worker_stack.txt
union/pl-transport-win.journal_bridge_contact.txt
union/pl-transport-win.lib.txt
union/pl-transport-win.observation_inertness.txt
union/pl-transport-win.ordinary_request_authority.txt
union/pl-transport-win.paired_state_compatibility.txt
union/pl-transport-win.relay_pairing_round_trip.txt
union/pl-transport-win.relay_round_trip.txt
union/pl-transport-win.transport_round_trip.txt
union/pl-transport-win.upload_lifecycle_log.txt
union/pl-transport-win.v3_header_callers.txt
```
