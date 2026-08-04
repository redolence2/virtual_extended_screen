# R1 evidence summary — locally verified (ladder state 3)

Date: 2026-08-04 · Executor: root reviewer (inline) · Base: candidate checkpoint `ce2d693`
Scope: `A00_REMEDIATION_PLAN.md` §5 R1 (a)–(e).

## (a) Exact ffmpeg pins — hash-chain evidence

- `Cargo.lock` SHA-256 **before** edits: `3bff3b84fe5e83b0c3e4ff063fa87e73aaa8600afb33e7aff4137b258a58c86d` (`r1-cargo-lock-before.txt`)
- Manifest edits: `ubuntu-client/Cargo.toml` → `ffmpeg-next = "=7.1.0"`, `ffmpeg-sys-next = "=7.1.3"`;
  `ubuntu-client/crates/video-decode/Cargo.toml` → same exact pins + T1-deletion quarantine comment
- `cargo metadata --locked` succeeded post-edit (lock satisfies the new requirements without change);
  all five workspace crates show `=7.1.0`/`=7.1.3` requirements, resolution 7.1.0/7.1.3
  (`r1-cargo-metadata-locked.txt`)
- `Cargo.lock` SHA-256 **after**: identical `3bff3b84…` — byte-for-byte unchanged (`r1-cargo-lock-after.txt`)
- Commit of both lockfiles: deferred to R7 per plan.

## (b) ERR-07 — ASCII-only profile strings

- `CONTRACT_ERRATA.md`: dated ERR-07 entry (rule, rationale incl. serializer-divergence hazard,
  parsed-document check ordered before the canonical-bytes comparison, fixture list, generator).
- `docs/WIRE.md` §9: NFC bullet replaced with the ASCII-only rule referencing ERR-07.
- Validators: Swift `CanonicalProfile.firstNonASCII` + `ProfileError.asciiViolation`, gate inserted
  before the canonicalize comparison; Rust `first_non_ascii` + ERR-07 error, same ordering.
- Fixtures (generator `tools/gen_profile_fixtures.py`, idempotent — run twice, second run
  "unchanged"): `profile_nonascii_raw.json` 503 B sha256-16 `53fecc78d03f0e25`;
  `profile_nonascii_escaped.json` 507 B sha256-16 `aade882e06feda2b`. Both carry the real
  `sw1-lowdelay` backend so nothing but the ASCII gate rejects the raw form.
- `proto/fixtures/README.md`: new "Profile fixtures" manifest section.

## (c) WIRE status governance

- Dated governance note added to `CONTRACT_ERRATA.md` **before** the WIRE edit (per WIRE's own
  governance header); `docs/WIRE.md` status → "Stage-1 candidate; freeze pending A0.0 gates and
  clean checkpoint".

## (d) Cross-language test evidence

- Swift `resc-fixture-check`: **131 ok / 0 FAIL** (was 126; +5: canonical-fixture ASCII pass,
  two `firstNonASCII` path unit checks, raw + escaped fixture rejections asserting the
  `asciiViolation` verdict specifically, violation at `profile_id`). Output: `r1-fixture-check.out`.
- Rust `cargo test -p diagnostics`: **15 passed / 0 failed / 1 ignored** (pre-existing tempdir
  ignore; was 12 passed; +3: `raw_nonascii_passes_sort_minify_but_fails_ascii_gate` — proves the
  raw fixture is byte-canonical under sort/minify and carries a valid backend, so rejection is
  attributable to ERR-07 alone — `escaped_nonascii_rejected_with_same_verdict`,
  `first_non_ascii_walks_keys_and_nested_values`). Output: `r1-cargo-test-diagnostics.out`.
- Same verdicts on the same fixture bytes in both languages (rejection + ERR-07 class +
  `profile_id` locus).
- Regression: `cargo test -p protocol` 49/49; full `swift build` clean (all targets).

## (e) Document reconciliation sweep

- `A00_IMPLEMENTATION_REPORT.md`: scope → ERR-01…07 + candidate wording; base-commit row updated to
  `ce2d693`; §2.1 heading → candidate-per-ERR-06; §6 heading → candidate; Doctor.swift 480→451;
  ~140→126 (two sites, with "count as of this report" note); 22-entry→23-entry; deviation §12.3
  cross-referenced to ERR-06; stale "everything is uncommitted" struck with dated resolution.
- `A00_IMPLEMENTATION_REPORT_response.md`: dated amendment block adopting the five-state ladder,
  correcting F8/F10 to partial states; §4 rows 1/2/9a re-worded to true states.
- Residual grep hits reviewed and intentional: banner's historical corrections list; "480 AUs"
  (sample length); "committed `mac-host/Sources/Protocol/`" (true since `ce2d693`); §12 deviation
  rows kept as historical record (harness JSON field gap closes in R3a).

## Ladder states after R1

F8 dependency pins → **locally verified** (commit at R7). F9 ASCII canonicalization →
**locally verified**. F10 report precision → **locally verified**. All other findings unchanged
(open, addressed by R2a–R6).
