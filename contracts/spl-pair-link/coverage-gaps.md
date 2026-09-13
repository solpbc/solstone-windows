<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<!-- Copyright (c) 2026 sol pbc -->

# SPL Pair-Link Coverage Gaps

Engineering-internal coverage report for vendored bundle `7.0.0` conformance against `observer-pl`.

## 1. Uncovered Predicates

Every one of the 48 named predicates in `vectors.json` `"covers"` is exercised by at least one driven vector in `crates/observer-pl/tests/spl_pair_link_conformance.rs`.

- **Uncovered count**: 0 of 48

## 2. Semantic Gap Vectors

Three `derive_jid` vectors are not driven due to intentional semantic gaps between the corpus specification and this repository's minimal pure P-256 parser contract:

1. **`identity.jid.compressed-point`**:
   - **Corpus expectation**: Encoding-invariant JID equal to canonical (`5620bab1-476a-88df-93d4-f4f525b991dd`).
   - **Implementation behavior**: `observer_pl::relay_window::jid_from_spki` hashes the presented SPKI DER directly via HKDF rather than decompressing and canonicalizing to an uncompressed point.
   - **Owning symbol**: `observer_pl::relay_window::jid_from_spki`
   - **Concrete next step**: Future enhancement to decompress SEC1 compressed points into uncompressed P-256 SPKI DER before HKDF expansion.

2. **`identity.jid.off-curve-point`**:
   - **Corpus expectation**: Error refusal (`result: "error"`).
   - **Implementation behavior**: `observer_pl::ca::is_ec_p256_spki` validates DER structure and algorithm/curve OIDs (`id-ecPublicKey` and `prime256v1`), but does not evaluate curve equation satisfaction for the public key point coordinates.
   - **Owning symbol**: `observer_pl::ca::is_ec_p256_spki`
   - **Concrete next step**: Future enhancement to validate public key coordinates satisfy the $y^2 \equiv x^3 - 3x + b \pmod p$ curve equation.

3. **`identity.jid.unused-bits`**:
   - **Corpus expectation**: Error refusal (`result: "error"`).
   - **Implementation behavior**: `observer_pl::ca::is_ec_p256_spki` inspects outer and inner AlgorithmIdentifier elements without asserting that the SubjectPublicKey BIT STRING unused-bits octet equals `0`.
   - **Owning symbol**: `observer_pl::ca::is_ec_p256_spki`
   - **Concrete next step**: Future enhancement to strictly parse the SubjectPublicKey BIT STRING header and assert zero unused bits.
