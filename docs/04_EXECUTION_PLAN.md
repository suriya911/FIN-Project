# Tessera — Execution Plan

Developed from `01_PROJECT_OVERVIEW.md`, `02_TECHNICAL_DESIGN.md`, `03_BUILD_PLAN.md`, and `CLAUDE.md`.
This is the working plan for building the project in this repository. Where it deviates from
`03_BUILD_PLAN.md`, the deviation and its reason are stated explicitly.

---

## 1. Analysis summary

The four documents are coherent and mutually reinforcing:

| Doc | Role |
|---|---|
| 01 — Project Overview | *Why*: the differentiator is provable correctness (determinism + differential fuzzing + realistic load), not the matcher. |
| 02 — Technical Design | *What*: concrete data structures (arena, flat levels, bitmap, cached best), the match loop, invariants I1–I10, the reference oracle. |
| 03 — Build Plan | *When*: 7 phases, dependency-ordered, ~3 weeks, with exit criteria. |
| CLAUDE.md | *Rules*: 22 absolute rules, repo layout, anti-patterns, final checklist. |

The build order is sound: **reference engine first** (it is the spec), fast engine second,
determinism third, then the fuzzer — which is the actual deliverable — then simulator,
performance, and polish. Nothing in the ordering needs to change.

### Issues found during analysis (resolved or flagged)

These are the points where the documents conflict, are underspecified, or collide with this
environment. Each has a decision.

**A. Market orders vs. price bounds check — spec conflict.**
`02 §2` says a market order uses `Price(i64::MAX/MIN)`, but `02 §6` step 1 rejects any price
that fails `price_to_idx` with `PriceOutOfBounds` *before* matching — which kills every market
order. **Decision: v1 is limit orders only (GTC/IOC/FOK).** A marketable IOC at the far
boundary price is behaviorally equivalent to a market order. Documented as a limitation.

**B. `no_std` timing.**
`03` defers `#![no_std]` to Phase 2; CLAUDE.md rule 4 says it is mechanically enforced.
Retrofitting `no_std` onto a `std` crate is pure churn. **Decision: `core` is `#![no_std]`
from the first commit.** Phase 2 then only has to *verify* (grep + compile), not convert.

**C. The output buffer is unspecified.**
`apply(&mut self, InputEvent, out: &mut EventBuffer)` needs a concrete no-alloc type.
**Decision:** `EventBuffer` is a caller-provided fixed-capacity buffer (capacity chosen at
startup ≥ worst-case events for one input: a full-book sweep can emit up to
`arena_capacity` fills + 1 terminal event). Overflow is a `debug_assert!` + saturating drop
in release — and an invariant the fuzzer watches.

**D. Proptest weights need a stateful generator.**
`03` Phase 3 says "~10% new, ~85% cancel, ~5% modify". A naive generator drawing random
`OrderId`s would make ~85% of ops `UnknownOrderId` rejects against an empty book — a tame
fuzzer. **Decision: the generator tracks live order IDs** and draws cancels/modifies mostly
from that set (with a small adversarial share of unknown/duplicate/foreign-trader IDs).

**E. FOK pre-check must be STP-aware.**
With `CancelResting`, the taker's own resting orders don't contribute fillable quantity.
`can_fill_fully(side, price, qty, trader, stp)` already takes both — the reference engine
must implement the *same* rule or the fuzzer drowns in false divergences. Written into the
Phase 0 test list (known-hard case 16).

**F. Environment-dependent items are best-effort.**
This is built in a cloud container: `perf`/`flamegraph`, core pinning (`taskset`/`isolcpus`),
frequency scaling control, and the cross-machine determinism check may be unavailable or
unrepresentative. **Decision:** structure the benches so they run anywhere; run the
`perf`-based measurements on real hardware when available and report the environment honestly
in the README. The cross-machine hash check becomes a CI job on a different runner OS.

**G. Reference engine will exceed "~200 lines".**
It must implement TIF, all four STP modes, all reject reasons, and byte-identical event
ordering. That's fine — the rule that matters is *obviously correct*, not the line count.
Never optimize it (rule 21).

---

## 2. Ground rules carried into every session

Before finishing any task, re-check CLAUDE.md "Absolute rules". The ones that bite in
practice, kept at hand:

- Integers only. `i64` ticks, `u64` lots. No floats outside sim price-walk & stats.
- Fill price = **resting** order's price.
- Cache `next` before `free()`.
- FOK pre-check mutates nothing.
- `Modify` = cancel + new; always loses priority.
- Level empties ⇒ clear bitmap bit ⇒ rescan `best` if it was best.
- `state_hash()` hashes semantics, never arena indices.
- `NIL = u32::MAX`. Zero is a valid index.
- No panic on any input-driven path — emit `Rejected`.
- Reference engine before fast engine; never optimize before Phase 5.

---

## 3. Phase plan

Same seven phases as `03_BUILD_PLAN.md`, with exit criteria promoted to hard gates.
"Session" = one focused working session in this repo; the plan is dependency-ordered,
not calendar-ordered.

### Phase 0 — Foundation & oracle *(gate: reference passes all unit tests)*

1. Workspace: `crates/core` (`#![no_std]`, zero deps), `crates/reference`, `crates/sim`,
   `crates/shell`, `crates/fuzz`, plus `benches/`.
2. `core/src/types.rs` + `events.rs` exactly per `02 §1–2` (minus market-order sentinel — see A).
3. **`crates/reference` — the oracle, written first.** Sorted `Vec` per side, linear scan,
   allocate freely.
4. ~25 hand-written unit tests against the reference: simple cross, partial fill, multi-level
   sweep, rest, cancel, cancel-nonexistent, cancel-wrong-trader, IOC partial, IOC full,
   FOK reject, FOK fill, FOK+STP interaction, all four STP modes, duplicate ID, zero qty,
   price out of bounds, modify loses priority, modify unknown ID.
5. CI (GitHub Actions): `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`
   on every push.

### Phase 1 — Fast engine *(gate: identical test suite passes; `validate()` green after every event)*

Implementation order: `arena.rs` → `book_side.rs` (flat levels, bitmap, cached best,
rescan with explicit u64-word-boundary tests) → `order_index.rs` (direct-mapped
`Vec<u32>`; shell guarantees dense IDs — documented loudly) → `book.rs` match loop →
`unlink_and_free` → `validate.rs` (I1–I10, debug-gated).
The 20+ Phase 0 tests run against both engines from a shared test harness (one test file,
generic over an `Engine` trait) so the suites can never drift apart.
**No optimization. None.**

### Phase 2 — Determinism *(gate: 1M-event log × 100 runs → identical outputs and state hash)*

1. Verify `no_std` (already true per B) — grep gate in CI: `Instant|SystemTime|rand|thread|Box|HashMap|BTreeMap|f32|f64` → 0 hits in `core/`.
2. `hash.rs` — canonical order: levels ascending, FIFO head→tail; hash
   `(order_id, price, remaining, trader)` + per-level aggregates. Never arena indices.
3. Journal: append-only binary log; `replay(log) -> (Vec<OutputEvent>, u64)`.
4. Snapshot/restore + fast-forward; test `snapshot_at(N) + replay_from(N) == full_replay()`.
5. Cross-OS hash check as a CI matrix job (Linux + macOS runner) — see F.

### Phase 3 — Correctness ★ THE PROJECT ★ *(gate: 8+ CPU-hours fuzzing, zero divergence, real bug list)*

1. Stateful proptest generator per D: tracks live IDs; weights ≈ 10% new / 85% cancel /
   5% modify *of meaningful operations*, plus an adversarial tail (unknown IDs, duplicate IDs,
   boundary prices, exact-fill quantities, same-trader flows, IDs colliding mod capacity).
2. Invariant suite: I1–I10 after **every** event; conservation, never-crossed,
   FIFO-priority properties.
3. `fuzz_targets/differential.rs` exactly per CLAUDE.md — event-stream equality after every
   input, `validate()` in debug.
4. The 15 known-hard cases from `02 §12` + case 16 (FOK×STP, per E) as explicit tests.
5. Long fuzz runs (overnight-equivalent, chunked if the environment requires). Every
   divergence → minimal repro → `tests/regressions/bug_NNN.rs` + entry in `docs/BUGS.md`
   (what broke, why, minimal repro).
6. Feed simulator logs (once Phase 4 lands) back into the differential harness.

### Phase 4 — Simulator *(gate: ~90% cancel rate, realistic book shape, same seed ⇒ byte-identical log)*

Seeded PCG (`rng.rs`, no `thread_rng`) → `Agent` trait → MarketMaker (the cancel-rate
driver), Momentum, Noise (Poisson), Adversarial → GBM/random-walk reference price
(floats allowed here only) → runner CLI (`--seed --agents --events --out`) → stats report
(order-to-trade ratio, cancel rate, depth, spread) sanity-checked against real
microstructure numbers.

### Phase 5 — Performance *(gate: latency histogram committed; zero allocations proven; failed-optimization log written)*

Baseline **first** (criterion + hdrhistogram: p50/p99/p99.9/p99.99/max). Panicking global
allocator proves zero steady-state allocation. Then one change at a time, measured:
`OrderSlot` field order, inlining, prefetch, rescan avoidance, branch hints, (feature-gated)
unchecked indexing after fuzz proof. Benchmarks that matter: latency vs. depth
(1k/10k/100k — must stay flat), cancel storm, adversarial load, cold vs. warm; throughput
reported last. `perf stat`/flamegraph on real hardware when available (per F); report the
measurement environment honestly.

### Phase 6 — Shell & polish *(gate: clone → one command → live book + histogram in <60s)*

SPSC ring (crossbeam), wire codec, journal writer, `ratatui` TUI (depth ladder, trades,
live latency histogram), CLI (`tessera sim | replay | bench`), README per CLAUDE.md
structure — **bug list first**, then latency, design rationale, determinism story,
"things that didn't work", honest limitations (single symbol, no market orders per A,
Modify loses priority, no networking). 30-second asciinema/GIF.

---

## 4. Scope guards

- **Never cut:** Phase 2 (determinism), Phase 3 (differential fuzzing).
- **Cut order if squeezed:** TUI → wire protocol/shell → adversarial agent → Modify.
- **Out of scope, permanently:** multi-symbol, market data feeds, FIX, networking, market
  orders (v1), `async`/`tokio`/locks anywhere near the core.

## 5. Definition of done

The final checklist in CLAUDE.md, verbatim — every box checked, with the grep gates and the
no-alloc proof wired into CI so they can't silently regress.
