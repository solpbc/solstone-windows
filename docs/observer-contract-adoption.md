# Observer-client authority bundle adoption

## Purpose and ownership

The observer-client authority bundle is a language-neutral wire and behavior
contract owned by the public `solstone-journal` repository. It is distinct from
this repository's generated AutomationId and state-token contract described in
`docs/automation-contract.md`.

The adopted authority revision is
`827d3761e2b515b9bd537ded28b245c8c6d86cc0`, bundle version `1.0.2`. Its
generator identity is `solstone.convey.contract.observer_bundle.v1`; its bundle
schema identity is `solstone.observer-client-contract-bundle.schema.v1`.

## Repository layout and path basis

Immutable exported bytes live under `contracts/observer-client/bundle/`. The
consumer-owned checked mirror is `contracts/observer-client/adoption.json`, and
the sole authored pin catalog and verifier live in `xtask::observer_contract`.

Every authority path uses one basis: it is relative to the authority bundle
directory supplied to the verifier. Therefore `authority_manifest_path` is
`manifest.json`, and the manifest file paths are `consumer-audit.json`,
`fixtures/wire-behavior.json`, `projection.openapi.json`, and `vectors.json`.
No pin is relative to `contracts/observer-client/` or the repository root.

The `xtask` crate is a dev-dependency of the two conformance-test crates so the
tests import the single pin catalog. Cargo dev-dependencies never propagate into
a dependent's normal or build graph; the application runtime graph excludes
`xtask` and the bundle.

## Local check

Run:

```text
make check-observer-contract
```

The command works offline with the locked dependency graph. It verifies file
inventory, safe regular paths, modes, digests, manifest/adoption values,
projection mappings, coverage sets, and focused behavior through the real Rust
wire types and local transport seams. It does not contact a live journal, prove
native packaging, or provide release or installed-artifact evidence.

## Re-vendoring ceremony

1. Start from a clean detached checkout of
   `https://github.com/solpbc/solstone-journal` at the exact reviewed commit.
2. Export to a fresh destination with:

   ```text
   .venv/bin/python scripts/export_observer_client_contract_bundle.py "$DESTINATION"
   ```

3. Run the authority repository's export verification before transporting the
   directory.
4. Produce the deterministic transport archive with the pinned recipe:

   ```text
   set -o pipefail; LC_ALL=C find "$BUNDLE" -mindepth 1 -printf '%P\0' | LC_ALL=C sort -z | tar --create --format=ustar --file=- --directory="$BUNDLE" --no-recursion --owner=0 --group=0 --numeric-owner --mtime=@0 --mode='u=rwX,go=rX' --null --files-from=- | gzip -n -9 > observer-client-contract.tar.gz
   ```

5. Compute the archive SHA-256 and byte length. Stop before extraction unless
   both match the reviewed pins.
6. Enumerate every member before extraction. Reject absolute or traversing
   paths, backslashes, control characters, duplicate or case-colliding names,
   non-portable names, links, devices, special types, executable files, and
   special permission bits.
7. Extract only into a fresh temporary directory. Independently hash
   `manifest.json`, compare the exact recursive inventory, and compare every
   declared file digest.
8. In a clean `solstone-windows` worktree, replace the entire vendored bundle
   root. Do not reformat, rename, regenerate, or add consumer files inside it.
9. Copy bytes only. Materialize directories as `0755` and files as `0644`; do
   not carry archive owner, group, mtime, or mode metadata.
10. Update the Rust pin constants after independent review, then update the
    checked adoption mirror. Never derive expected operation or fixture/vector
    coverage from the candidate bundle.
11. Run `make check-observer-contract`, focused crate tests, the host checks,
    and the separately required native Windows evidence.

## Version policy

- Patch releases may carry compatible authority corrections, but still require
  complete pin replacement and review.
- Minor releases require explicit review of additive operations,
  fixtures/vectors, and the Windows adoption selection.
- Major or protocol-breaking releases require a separately approved migration
  design. Do not add an automatic fetch or compatibility shim.

## Bytes and provenance

The verifier can prove that committed bytes equal reviewed SHA-256 pins. It
cannot prove that an arbitrary directory came from a particular Git commit.
Association with the authority commit is established by the clean detached
checkout, verified export, deterministic archive, and review ceremony above;
the digests then preserve the identity of those bytes.
