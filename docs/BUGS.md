# Bug log

Running log of every defect found while building Tessera, what found it,
and where the regression lives. Honesty over drama: the differential
fuzzing campaign on the finished engine found **zero divergences** — and
the second half of this file shows the work done to prove that result is
meaningful rather than vacuous.

---

## 1. Defects found during development

### BUG-001 — Spec conflict: market orders vs. price-bounds validation
- **Found by:** design review (before any code), while reconciling
  `02_TECHNICAL_DESIGN.md` §2 with §6.
- **What was wrong:** the design says a market order is
  `Price(i64::MAX/MIN)`, but the match loop's validation rejects any
  price failing `price_to_idx` — which would reject every market order
  as `PriceOutOfBounds` before matching.
- **Resolution:** v1 is limit orders only; a marketable IOC at the far
  grid boundary is behaviorally equivalent. Recorded as a limitation.
  (`docs/04_EXECUTION_PLAN.md`, issue A.)

### BUG-002 — FOK pre-check needed STP-awareness or the engines diverge
- **Found by:** design review of the FOK/STP interaction.
- **What was wrong:** a naive FOK pre-check sums all crossing quantity.
  Under `CancelResting` the taker's own orders are cancelled, not
  filled (they contribute nothing); under `CancelAggressor`/`CancelBoth`
  the taker dies at its own order, so quantity behind it is unreachable.
  Two engines disagreeing on this definition = permanent false
  divergences.
- **Resolution:** `can_fill_fully` is STP-aware in both engines, with
  the rule written down once. Regression: `hard_cases::case_16` and
  `suite::fok_stp_cancel_aggressor_blocks`.

### BUG-003 — Level quantity aggregates could overflow u64
- **Found by:** design review while sizing `Level::total_qty`.
- **What was wrong:** two resting orders of ~`u64::MAX` at one level
  overflow a u64 sum; release-mode wraparound would silently corrupt
  the FOK pre-check and invariant I5.
- **Resolution:** all quantity aggregates (level totals, FOK sums,
  conservation ledgers) are u128. The generators throw `u64::MAX`
  quantities specifically to patrol this.

### BUG-004 — Test-harness bug: i8 overflow produced an invalid proptest range
- **Found by:** the property suite panicking inside proptest on its
  first run.
- **What was wrong:** `-2i8..(CFG.num_levels as i8 + 2)` with
  `num_levels = 128`: `128 as i8` wraps to `-128`, producing an empty
  range. The bug was in the *test harness*, not the engine — but it
  would have silently weakened the generator had it not panicked.
- **Resolution:** offsets widened to i16 (`properties.rs`). A reminder
  that harness code needs the same paranoia as engine code.

### BUG-005 — Pinned state-hash constant mistyped on first commit attempt
- **Found by:** `determinism::state_hash_pinned_value`, immediately.
- **What was wrong:** transcription error converting the expected hash
  to hex. Caught before it ever reached the branch; kept here because
  it demonstrates the pinned-constant test doing its job (it will catch
  real canonical-encoding drift the same way).

### BUG-006 — Reference match loop carried a dead scan index
- **Found by:** clippy (`unused_mut`) after the first oracle version.
- **What was wrong:** not a behavior bug — the scan index could never
  advance (every iteration either consumes the front order or exits),
  so the variable was constant and misleading in the one file whose only
  job is to be obvious.
- **Resolution:** rewritten to read `opp[0]` explicitly with a comment
  stating the front-consumption invariant.

---

## 2. The fuzzing campaign (zero divergences — and why that's credible)

Campaign on the finished engine, all on this repo's code:

| Harness | Volume | Result |
|---|---|---|
| `difftest` (stable, seeded StreamGen, 3 book shapes) | 14,283 seeds × 20,000 events × 3 configs ≈ **857M events** | zero divergences, zero invariant violations |
| `cargo fuzz run differential` (libFuzzer, ASan) | 2.7M structured inputs across multiple sessions | zero crashes/divergences |
| proptest `engines_agree` (+5 more properties) | 512 shrinking cases per property per run | all pass |
| 1M-event log × 100 replays (release) | 100M events | byte-identical outputs + state hash |

Why zero? The classic bugs this fuzzer exists to find (fill at the
aggressor's price, missing bitmap clear, freed-slot `next` reuse,
zero-qty ghosts) were named in the design docs *before implementation*,
and the reference engine was written first as the executable spec —
so those bugs were mostly never typed in. That is the intended effect
of the process, not luck.

### Mutation validation: proving the harness has teeth

A fuzzer that finds nothing could just be blind. To rule that out, each
classic bug was deliberately injected into the fast engine and the
harness had to catch it:

| Mutation (classic bug) | Detected | How |
|---|---|---|
| M1: fill emitted at the wrong price (maker+1 tick) | **event 1, seed 0** | output divergence |
| M2: bitmap bit not cleared when a level empties | **event 6, seed 0** | invariant I3 |
| M3: FOK pre-check ignores STP | event 2,872, seed 0 | output divergence |
| M4: fully-filled maker left as zero-qty ghost | seconds (livelock) | cross loop spins on the ghost; caught by hang + qty-0 fill divergence |

Every mutation was reverted after detection (`git checkout`); the
mutations never touched the branch history.

M4's detection mode is worth a note: a zero-qty ghost doesn't just
diverge, it *livelocks* the cross loop (the ghost keeps the level "best"
while contributing no fillable quantity). Invariant I9 ("no zero-qty
resting orders") exists precisely so the validator reports this state
before the loop can spin.

---

## 3. Standing instructions

Every future divergence found by any harness gets:
1. a minimal reproducer committed under `crates/tests/tests/regressions/`,
2. an entry in section 1 of this file: what broke, root cause, repro,
3. the fix, in a separate commit from the reproducer where practical.
