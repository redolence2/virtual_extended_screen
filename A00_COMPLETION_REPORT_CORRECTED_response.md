# Response to the Corrected-Completion-Report Review

| | |
|---|---|
| **Date** | 2026-08-05 |
| **Responds to** | `A00_COMPLETION_REPORT_CORRECTED_review.md` — SHA-256 `fcf79e853901a483a81083b03817826e5908d16d948731f71f8c7ea50d630276` |
| **Report under review** | `A00_COMPLETION_REPORT_CORRECTED.md` — SHA-256 `a46615c8032f505fe0aa08448ccbae78463cae2ffcb530359679a55ea6ec719b` (hash-pinned by the review; therefore NOT edited — its corrections are recorded in §3 here and fold into the future R′) |
| **Commits** | C `8e7170f` · E `5755c7a` · R `b747c24` (HEAD; tree clean apart from the review + this response) |

## 1. Verdict — accepted in full, both halves

- **Demo GO accepted.** The one remaining demo gate is the review's single manual real-pair
  smoke test (§4 below). No further matrix, evidence commit, or review round before the demo.
- **Formal State-5 NO-GO accepted without dispute.** Every one of the five deferred findings is
  verified real (§2). A0.0 remains incomplete; Stage 1 remains *candidate*; A0 and T1 remain
  NO-GO. The C→E→R chain stands as immutable history; formal completion, if pursued later, is
  the appended C′→E′→R′ chain per the review's sequence — no new plan or response-review round
  before that bounded work starts.

## 2. Independent verification of the review's findings — all confirmed

Performed today from R (`b747c24`), before accepting anything:

| # | Review claim | My check | Result |
|---|---|---|---|
| 1 | Client never stops frame intake before final drain; drain is one `try_recv()` snapshot; `decoder.flush()` failure is only logged, excluded from the clean/aborted decision | Read `ubuntu-client/src/main.rs` shutdown block + `video_receiver.rs` run loop (no stop flag — exits only on channel disconnect/socket error) | **Confirmed** — an admitted frame enqueued after the last `try_recv()` escapes ledger and footer; flush `Err` + empty ledger still yields `status="clean"` |
| 2 | R4 footers carry different run tokens; joiner checks neither token equality, footer-last, footer counters, nor count reconciliation; selftest codifies differing default tokens; causal check runs on joined pairs only; runner proves disappearance, not exit codes, and has an unconditional `pkill -9` EXIT trap | Read sealed traces (host line 77: `88f95fc7bb4d8409`; client line 131: `fda759bdc737c624`); `join_trace.py` `find_footer` (position-independent, per-side, status-only) and selftest defaults (`0123456789abcdef` vs `fedcba9876543210`); causal loop iterates `joined_by_id`; `r4_live_gate.sh` wait helpers poll `kill -0`, never collect exit codes | **Confirmed** on every point |
| 3 | Outbound vector matrix omits the 8 legal TraceOrDoctor clock sends at `profile_accepted`/`video_ack_accepted` (2 phases × 2 roles × 2 kinds) | Read `gen_dispatch_fixtures.py`: 156-row base is `diagnostics="normal"` only; TraceOrDoctor clock specials exist only at `active` | **Confirmed** — fixture-completeness gap, dispatch design unaffected |
| 4 | r3b doctor JSONL not retained/token-bound; the three historical `r3b-host-doctor-{1,2,3}.json` are malformed JSON | `json.load` on all three → all fail (console log text precedes the JSON) | **Confirmed** — they are v1-runner captures (stdout+report concatenated); rename to `.log` or omit in E′ |
| 5 | Manifest is not a validating 13-gate seal: selective parsing (3 malformed `.json` hashed anyway), hard-coded gate rows/exits, unused `token_line`, no expected-artifact inventory, no verify mode, missing Swift/SDL env; **generator added in E, not C** — the "all corrective tooling in C" attestation is false as stated | `git log --follow tools/gen_evidence_manifest.py` → sole commit `5755c7a` (E); source re-read confirms each mechanism claim | **Confirmed, including the topology defect** — the generator exempted itself from the C→HEAD diff and report line 8 did not disclose it |
| — | Causal-lead provenance | Recomputed: final sealed host trace = 72 frames, **30 leading, max 9,003 µs**; wip trace as committed at C = 51 frames, **24 leading, max 9,190 µs** | **Matches the reviewer's recomputation exactly**; both < 16,667 µs |
| — | ERR-09 | `grep ERR-09 CONTRACT_ERRATA.md` → absent | **Confirmed** — report line 50 was wrong as written |

## 3. The four report corrections — accepted and restated

The reviewed report is hash-pinned, so the corrected statements live here and become part of R′:

1. **ERR-09**: *the ERR-09 escape path was implemented but not used; ERR-09 was not written.*
   No equivalence erratum exists in `CONTRACT_ERRATA.md`; real zero-output was forced on
   sw1-lowdelay, so none was needed. (Line 50's "written-but-never-needed" conflated
   implementing the escape hatch with writing the erratum.)
2. **Runner qualification**: `r4_live_gate.sh` is *graceful-termination-requesting* but not yet
   strictly graceful-only — it retains an unconditional SIGKILL EXIT trap and proves process
   disappearance rather than collecting exit codes. The unqualified phrase is withdrawn until
   the D2 hardening lands.
3. **Causal provenance**: the 16,667 µs (one 60 Hz frame period) bound stands. The **9,190 µs**
   maximum belongs to C's earlier retained WIP run (24 of 51 frames leading); the **final sealed
   run's** maximum is **9,003 µs** (30 of 72 leading). Both inside the bound.
4. **Gate wording**: "13 gates, all PASS" describes gates *executed and reported*; only M3, X1,
   and X2 currently bind raw outputs into the seal. "Validator-proven" is withdrawn until E′
   derives every PASS from its retained execution record.

## 4. The demo path (the only open gate before release)

One precision note first — not a dispute of the GO: the sealed live-pair runs and the normal
client path decode via the **legacy auto-selection: `hevc_cuvid` first, software fallback**
(`ubuntu-client/crates/video-decode/src/lib.rs:74`), not `sw1-lowdelay`. The sw1-lowdelay
evidence comes from the doctor / decoder-experiment / harness paths. The demo therefore ships
on CUVID as-is — consistent with the review's "hard-coded personal settings, code as-is" smoke
test, and already recorded as the A0-preparation item (the A0 *measurement* path must pin
sw1-lowdelay; the demo need not).

Smoke-test execution plan, mapping the review's five steps (I orchestrate; the user observes the
box's monitor and drives its mouse/keyboard — the two visual/interactive confirmations are the
user's by design):

1. **Start** — Mac: `./.build/debug/remote-display-host --client 192.168.50.47 --hevc` (no
   `RESC_TRACE`); box: `DISPLAY=:0 LD_LIBRARY_PATH=$HOME/ffmpeg7/lib ./target/release/remote-display-client -H 192.168.50.125`
   from `~/resc/remote_extended_screen/ubuntu-client`, both at the recorded demo commit.
2. **Motion** — drag a window / play motion on the Mac's virtual extended display; user confirms
   the Ubuntu monitor updates live.
3. **Input** — user moves the box's mouse / types; confirms the pointer and keys act on the Mac
   extended screen as intended for the demo.
4. **Restart** — stop both once (documented commands only), restart, confirm recovery with no
   manual cleanup beyond those commands.
5. **Retain** — keep the ordinary host/client logs from both phases and record the exact commit
   in a short demo note (commit, date, pass/fail per step).

If all five pass, the demo is released per the review — no further process.

## 5. Deferred formal-hardening queue (accepted verbatim; NOT scheduled)

Recorded so a future session can execute without re-deriving. One bounded pass, only if formal
State 5 is later wanted; then C′ (all tooling in-checkpoint, including the finished
generator/validator) → full matrix from C′ → E′ (validated evidence, no tooling change) → R′
(attestation of C′+E′, incorporating §3's corrections) → one independent re-review.

- **D1** — receiver stop flag (`Arc<AtomicBool>` on the existing 100 ms socket timeout); set
  before drain; drain to channel disconnection, not a snapshot; flush/EOF success required for
  `clean`; negative tests: duplicate submit/recovery, ledger overflow, missing recovery,
  unresolved EOF, producer/drain race, flush failure.
- **D2** — runner-generated single run token passed to both endpoints; joiner requires token
  equality, footer-last, zero footer failure/pending counts, footer↔record count reconciliation;
  host-local causal check on **every** host frame with real capture identity (drops included);
  runner collects real host/client exit codes (ssh/poll failure = failure), pid-targeted
  cleanup only, disabled after graceful success; selftests for each new failure mode.
- **D3** — add the 8 missing TraceOrDoctor outbound rows, regenerate, run both language
  consumers.
- **D4** — token-bound r3b: isolated/token-tagged doctor logs both ends, raw doctor JSONL
  retained, exactly-one token-matching `doctor_complete` per retained log, all reports parsed,
  filenames bound to the runner summary; drop or rename the three malformed historical doctor
  files.
- **D5** — validating manifest: in C′; explicit per-gate required-artifact inventory;
  machine-readable execution record per gate (commit, dirty state, machine, command, cwd,
  start/end, exit); full env incl. Swift toolchain/SDK and SDL; parse every `.json`/`.jsonl`;
  every PASS derived from retained output; require `strategy_used != "not_forced"` while ERR-09
  is absent; read-only verify mode (recompute + compare, fail on missing/changed/extra/
  malformed/semantically-failed); semantic replay of the strict R4 join + token-bound r3b set.
  Non-self-reference principle unchanged: E′'s manifest records C′, never its own commit.

## 6. Position after this response

Corrective chain C→E→R: sealed history, verdict recorded. **Demo: GO pending the §4 smoke
test** (needs the user at both screens; I run it on their word). **Formal A0.0 / State 5:
open, deferred** — D1–D5 + C′→E′→R′ whenever the user chooses to resume it. Nothing else is
owed to any reviewer loop before the demo.
