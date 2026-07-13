# CLAUDE.md

Instructions for an LLM building **Tessera**: a deterministic, replayable limit order book matching engine with an agent-based market simulator and a differential fuzzer.

Read this file **completely** before writing any code. Re-read the "Absolute rules" section before every task.

---

## What we are building and why

A limit order book matching engine in Rust. Price-time priority. In-memory. Single-threaded core.

**The matching algorithm is not the point.** It is 60 years old and solved. The point is everything around it:

1. **Bit-for-bit determinism** — same input log → same output log → same state hash. Always.
2. **A differential fuzzer** — a slow, obviously-correct reference engine races the fast one. Divergence = bug.
3. **An agent-based simulator** — realistic load (90% cancel rates, real book depth), not `random_order()` in a loop.

The success criterion is not "it's fast." It is **"it can prove it's correct, and here are the bugs the proof found."**

Optimize for: correctness you can demonstrate, determinism you can verify, and performance you can *explain*. In that order.

---

## Absolute rules

Violating any of these is a bug, not a style disagreement. **Check every one before you finish a task.**

### Money

1. **NEVER use `f32` or `f64` for prices, quantities, or money. Ever.** Prices are `i64` ticks. Quantities are `u64` lots. If you catch yourself typing `as f64` in engine code, stop and reconsider what you're doing.
2. Never use floats for intermediate calculations either. `total_qty`, `notional`, fill sums — all integer.
3. Floats are permitted in exactly two places: (a) the simulator's reference-price random walk, and (b) benchmark/statistics reporting. Nowhere else.

### The engine core

4. **The `core` crate is `#![no_std]`.** This is mechanically enforced. It cannot call the clock, the RNG, or the OS.
5. **No `Instant::now()`, `SystemTime`, `rand`, `thread_rng`, or any clock or RNG inside the engine.** Time is the input event's `Seq` number and nothing else. If a decision depends on wall-clock time, the design is wrong.
6. **No allocation in the hot path.** The arena and level array are pre-allocated once at startup. `apply()` must never allocate. Prove it with the panicking allocator.
7. **No `Box`, `Rc`, `Arc`, `Vec::push` (in hot path), `HashMap`, or `BTreeMap` in the engine.** Arena + `u32` indices. Flat arrays. Bitmaps.
8. **No threads, no locks, no `async`, no `tokio` inside the core.** Single-threaded state machine. Concurrency lives in the shell.
9. **No `unwrap()` / `expect()` / `panic!()` on any input-driven path.** Bad input produces a `Rejected` event. The engine never dies because someone sent a weird order. (`debug_assert!` is fine and encouraged — it compiles out in release.)
10. **`u32::MAX` is `NIL`.** It is the universal sentinel for "no order" / "no level." Never use `0` as a sentinel — `0` is a valid index.

### Correctness

11. **A fill always executes at the RESTING order's price.** The aggressor receives price improvement. Getting this backwards is the most common bug in a first implementation.
12. **Cache `next` before freeing a slot.** `free()` pushes the slot onto the free-list, which overwrites `next`. Read it first, always.
13. **FOK must pre-check without mutating.** Walk the levels, sum the available quantity, and only *then* decide. A rejected FOK must leave the book **byte-identical** to how it was before.
14. **`Modify` = cancel + new. It always loses time priority.** This is a deliberate design decision. Document it; do not "fix" it.
15. **When a level empties, clear its bitmap bit AND rescan `best` if it was the best level.** Forgetting either is a bug the fuzzer will find in seconds.

### Determinism

16. **`state_hash()` hashes semantics, not layout.** Hash live orders in canonical order (levels ascending, then FIFO head→tail) and level aggregates. **Never hash arena indices or the free-list** — two deterministic runs may legitimately place the same logical book in different physical slots.
17. **Every `OutputEvent` carries the `Seq` of the input that caused it.** This makes output logs self-describing and diffable.
18. **The simulator uses seeded PRNGs only.** `--seed 42` must always produce a byte-identical market. No `thread_rng`.

### Process

19. **Write the reference engine BEFORE the fast engine.** It is the specification. If you write the fast one first, you will unconsciously bend the reference to agree with it and the fuzzer becomes worthless.
20. **Never optimize before Phase 5.** No `perf` data means any optimization is a guess. Correct first, measure second, optimize third.
21. **Never optimize the reference engine.** Its only job is to be obviously correct. If you're adding an index to it, stop.
22. **Every bug the fuzzer finds becomes a named regression test** in `tests/regressions/`, with a comment explaining what broke and why.

---

## Repository layout

```
tessera/
├── CLAUDE.md                    ← this file
├── README.md                    ← the deliverable. Bug list goes at the top.
├── Cargo.toml                   ← workspace
│
├── crates/
│   ├── core/                    ← #![no_std]. THE ENGINE. Zero dependencies.
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── types.rs         Price, Qty, OrderId, Seq, Side, TimeInForce, STP
│   │   │   ├── events.rs        InputEvent, OutputEvent, CancelReason, RejectReason
│   │   │   ├── arena.rs         OrderSlot, free-list
│   │   │   ├── book_side.rs     flat level array + occupancy bitmap + cached best
│   │   │   ├── order_index.rs   OrderId → arena idx (flat map, NOT std HashMap)
│   │   │   ├── book.rs          OrderBook::apply() — the match loop
│   │   │   ├── validate.rs      invariants I1–I10, debug-gated
│   │   │   └── hash.rs          state_hash()
│   │   └── tests/
│   │       ├── unit.rs
│   │       └── regressions/     ← one file per fuzzer-found bug
│   │
│   ├── reference/               ← THE ORACLE. Slow. Obvious. ~200 lines.
│   │   └── src/lib.rs           Vec + sort + linear scan. Allocate freely.
│   │
│   ├── sim/                     ← agent-based market simulator
│   │   ├── src/
│   │   │   ├── agent.rs         trait Agent
│   │   │   ├── market_maker.rs
│   │   │   ├── momentum.rs
│   │   │   ├── noise.rs
│   │   │   ├── adversarial.rs
│   │   │   ├── rng.rs           seeded PCG/xorshift. NO thread_rng.
│   │   │   └── runner.rs
│   │
│   ├── shell/                   ← I/O, clock, sequencing, wire protocol, TUI
│   │   └── src/
│   │       ├── journal.rs       append-only input log
│   │       ├── ring.rs          SPSC ring buffer
│   │       ├── codec.rs         wire encode/decode
│   │       └── tui.rs           live book view (ratatui)
│   │
│   └── fuzz/                    ← cargo-fuzz targets
│       └── fuzz_targets/
│           └── differential.rs  ← THE IMPORTANT ONE
│
├── benches/
│   ├── throughput.rs            criterion
│   ├── latency.rs               hdrhistogram: p50/p99/p99.9/p99.99
│   ├── depth_scaling.rs         latency vs. 1k/10k/100k resting orders
│   └── cancel_storm.rs          latency at 90% cancel rate
│
└── docs/
    ├── DESIGN.md
    └── BUGS.md                  ← running log of every fuzzer find
```

**Crate dependency rules:**
- `core` depends on **nothing**. Not even `std`.
- `reference` depends on `core` (for the types only).
- `sim`, `shell`, `fuzz`, `benches` may depend on whatever they need.
- **Nothing in `core` may ever depend on `sim` or `shell`.**

---

## Build order

Follow this exactly. Each phase has an exit criterion. **Do not proceed until it's met.**

### Phase 0 — Foundation (1–2 days)

1. `cargo new` the workspace with the crates above.
2. `crates/core/src/types.rs` — all core types. Derive `Copy, Clone, PartialEq, Eq, Debug, Hash`.
3. `crates/core/src/events.rs` — `InputEvent`, `OutputEvent`, reason enums.
4. **`crates/reference/src/lib.rs` — WRITE THIS NOW, BEFORE THE FAST ENGINE.**
   - `Vec<RefOrder>` per side, kept sorted (bids: price DESC then seq ASC; asks: price ASC then seq ASC).
   - `apply()`: linear scan the opposite side, fill greedily while it crosses, `retain(|o| o.remaining > 0)`, then push + sort if it rests.
   - Allocate freely. Sort every time. Clone whatever. **It is correct, and that is its only job.**
5. Twenty hand-written unit tests: simple cross, partial fill, multi-level sweep, cancel, cancel-nonexistent, IOC partial, FOK reject, FOK fill, self-trade all four modes, duplicate ID, zero qty, price out of bounds.
6. CI: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`.

**Exit:** the reference engine passes all 20 tests.

### Phase 1 — Fast engine (3–5 days)

Implement, in this order:

1. `arena.rs` — `OrderSlot` (hot fields FIRST: `remaining`, `order_id`, `next`, `prev`, `price`, `trader`, `level_idx`), free-list reusing `next` as the link, `with_capacity` / `alloc` / `free`. `alloc` returns `None` when full → `RejectReason::ArenaFull`.
2. `book_side.rs` — flat `levels: Vec<Level>`, `occupied: Vec<u64>` bitmap, cached `best: u32`.
   - `price_to_idx` with bounds checks → `Option<u32>`. **Never panic. Never wrap.**
   - `rescan_best_bid`: mask off bits ≥ `from`, scan words downward, `63 - leading_zeros()`.
   - `rescan_best_ask`: mask off bits ≤ `from`, scan words upward, `trailing_zeros()`.
   - **Test the `u64` word boundary explicitly** (level 63↔64). This is where the off-by-one lives.
3. `order_index.rs` — direct-mapped `Vec<u32>` if IDs are dense; otherwise a hand-rolled linear-probed flat map, power-of-two sized, pre-allocated, never grows, load factor ≤ 0.5, tombstones on delete. **Not `std::HashMap`.**
4. `book.rs` — `apply()`:
   - Validate (zero qty, dup ID, price bounds) → `Rejected`.
   - FOK pre-check (walk + sum, **no mutation**) → `Rejected` if unfillable.
   - Emit `Ack`.
   - Cross loop: `while remaining > 0 && best != NIL && crosses(...)` → `fill_at_level`.
   - Rest (GTC) / kill (IOC).
5. `fill_at_level` — walk the FIFO head→tail:
   - **Cache `next` before anything else.**
   - Self-trade prevention check.
   - `fill_qty = min(remaining, maker.remaining)`.
   - **Emit `Fill` at the MAKER's price.**
   - Decrement both; decrement `level.total_qty`.
   - If `maker.remaining == 0` → `unlink_and_free`.
6. `unlink_and_free` — splice the doubly-linked list; if the level hits `order_count == 0`, clear the bitmap bit and rescan `best` if it was the best.
7. `validate.rs` — invariants I1–I10. `#[cfg(debug_assertions)]`. Call after every event in tests.
8. **Run the same 20 tests from Phase 0 against the fast engine.**

**Exit:** the fast engine passes the identical test suite. `validate()` passes after every event.

> **Do not optimize here.** Correct structure first. You have no profiler data, so every "optimization" would be a guess.

### Phase 2 — Determinism (1–2 days)

1. Add `#![no_std]` to `core`. Fix the fallout.
2. Grep `core/` for: `Instant`, `SystemTime`, `rand`, `thread`, `Box`, `HashMap`, `BTreeMap`, `f32`, `f64`. **All must be zero hits.**
3. `hash.rs` — `state_hash()`. Canonical iteration: levels ascending, FIFO head→tail. Hash `(order_id, price, remaining, trader)` per live order and `(idx, total_qty, order_count)` per level. **Do NOT hash arena indices or the free-list.**
4. Journal: append-only binary log of `InputEvent`.
5. `replay(log) -> (Vec<OutputEvent>, u64)`.
6. Snapshot/restore: serialize book state, restore, fast-forward from event N.
7. Test: run a 1M-event log 100× → identical outputs and identical state hash.
8. Test: `snapshot_at(N) + replay_from(N) == full_replay()`.

**Exit:** `replay_is_deterministic` passes 100 iterations.

### Phase 3 — Correctness ★ THE PROJECT ★ (3–4 days — do not rush)

1. `proptest` generators: weighted event streams (~10% new, ~85% cancel, ~5% modify — realistic ratios).
2. `proptest` invariant suite: check I1–I10 after **every single event**.
3. Properties:
   - Conservation: `Σ fills + Σ remaining + Σ cancelled == Σ submitted`.
   - The book never crosses.
   - FIFO priority: at the same price, the older order always fills first.
4. `crates/fuzz/fuzz_targets/differential.rs`:

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use arbitrary::Arbitrary;

#[derive(Arbitrary, Debug)]
struct Ops(Vec<RawOp>);   // RawOp: a structured, arbitrary-derivable event

fuzz_target!(|ops: Ops| {
    let mut fast = tessera_core::OrderBook::new(CONFIG);
    let mut refr = tessera_reference::ReferenceBook::new(CONFIG);

    for (i, raw) in ops.0.iter().enumerate() {
        let ev = raw.to_input_event(Seq(i as u64));

        let mut fast_out = Vec::new();
        fast.apply(ev, &mut fast_out);
        let ref_out = refr.apply(ev);

        assert_eq!(
            fast_out, ref_out,
            "DIVERGENCE at event {}: {:?}\n  fast: {:?}\n  ref:  {:?}",
            i, ev, fast_out, ref_out
        );

        #[cfg(debug_assertions)]
        fast.validate().expect("invariant violated");
    }
});
```

5. Write the 15 known-hard cases from the design doc as explicit tests.
6. **Run the fuzzer for 8+ hours:** `cargo fuzz run differential -- -max_total_time=28800`
7. Fix every divergence. **Commit each as a named regression test.** Log it in `docs/BUGS.md`: what broke, why, minimal repro.

**Exit:** 8+ CPU-hours, zero divergence. A real bug list.

> **If the fuzzer finds nothing in 8 hours, your generator is too tame.** Crank up the adversarial weight: same-trader orders (to hit STP), boundary prices (`min_price`, `max_price`), exact-fill quantities, cancel-immediately-after-fill, IDs colliding mod capacity, a single level with thousands of orders.
>
> **The bug list is the deliverable.** It is the single strongest thing in this project. Do not shortchange this phase.

### Phase 4 — Simulator (2–3 days)

1. `rng.rs` — seeded PCG or xorshift. **No `thread_rng` anywhere.**
2. `Agent` trait: `fn act(&mut self, view: &BookView, rng: &mut Rng) -> Vec<InputEvent>`.
3. `MarketMaker` — quote both sides around a reference price; cancel + requote on drift. **This is what generates the realistic ~90% cancel rate.**
4. `Momentum` — cross the spread on directional flow.
5. `Noise` — Poisson-arrival random orders.
6. `Adversarial` — hammer one level; submit-cancel tight loops; orders at book edges; exact-fill quantities.
7. Reference price: geometric Brownian motion or a simple random walk. (Floats are fine *here*.)
8. Runner: `tessera sim --seed 42 --agents 100 --events 10M --out log.bin`.
9. Report stats: order-to-trade ratio, cancel rate, mean depth, spread distribution. **Sanity-check against real market microstructure numbers.**

**Exit:** ~90% cancel rate, realistic book shape, same seed → byte-identical log.

### Phase 5 — Performance (3–5 days)

**Baseline first. Record the numbers before touching anything.**

1. `criterion` throughput bench. `hdrhistogram` latency bench (p50/p99/p99.9/p99.99/max).
2. `perf stat -e cycles,instructions,cache-misses,cache-references,branch-misses`.
3. `flamegraph`. Find the real hot path. It will surprise you.
4. Pin to an isolated core (`taskset`), disable frequency scaling, `nice -20`. **Without this your p99.99 numbers are fiction.**
5. Enable the panicking allocator → **prove zero allocation in steady state:**

```rust
#[cfg(feature = "no-alloc-check")]
struct PanicAllocator;
#[cfg(feature = "no-alloc-check")]
unsafe impl GlobalAlloc for PanicAllocator {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 { panic!("allocation in hot path"); }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}
```

Flip it on *after* startup allocation, run the bench, and if it doesn't panic you have proof.

Then, **one change at a time, measuring each:**

- Reorder `OrderSlot` fields → hot fields in the first cache line.
- `#[inline(always)]` on `price_to_idx`, `crosses`, FIFO link/unlink.
- Prefetch the next order in a level walk (`_mm_prefetch` on the cached `next`).
- Skip the bitmap rescan when the best level doesn't empty.
- Branch hints on rare paths (reject, arena-full, STP).
- (Optional, risky) `get_unchecked` in the level walk — feature-gated, and only after the fuzzer has proven the bounds.

Then benchmark what matters:

- **Latency vs. book depth** (1k / 10k / 100k resting) — **it should stay FLAT.** That's the whole point of the design.
- **Latency under cancel storm** (90% cancel rate).
- **Latency under adversarial load** (everything at one level).
- Cold vs. warm. Report honestly.
- Throughput (orders/sec) — the headline. **Report it LAST.**

**Exit:** a latency histogram in the README. Sub-1 cache miss per order. Zero allocations proven. **A written record of every optimization tried — including the failures.**

> **Write down what didn't work.** *"I tried prefetching the next level's head pointer; it was 3% slower because it evicted the current level's FIFO from L1. Here's the `perf` output."*
>
> That paragraph beats a list of tricks that happened to work. It proves you measure instead of cargo-cult.

### Phase 6 — Shell & polish (2–3 days)

1. SPSC ring buffer (`crossbeam` or hand-rolled).
2. Wire codec: `InputEvent` / `OutputEvent` encode + decode.
3. Journal writer → real sessions become replayable.
4. **TUI live book view (`ratatui`)** — depth ladder, last trades, live latency histogram. *This is what makes it demo-able in 30 seconds.*
5. CLI: `tessera sim`, `tessera replay`, `tessera bench`.
6. README (see below).
7. asciinema / GIF of the TUI under load.

**Exit:** clone → one command → live order book + latency histogram in under 60 seconds.

---

## README structure

The README **is** the deliverable. An interviewer spends 90 seconds on it. Structure accordingly.

```markdown
# Tessera

A deterministic limit order book that can prove it's correct.

[GIF of the TUI under simulated load]

## Differential fuzzing found 14 bugs in my own engine

| # | Bug | Root cause | Repro |
|---|-----|-----------|-------|
| 1 | Fill executed at aggressor's price | ... | tests/regressions/bug_001.rs |
| 2 | Bitmap bit not cleared on exact fill | ... | tests/regressions/bug_002.rs |
| ... |

[← LEAD WITH THIS. It is the strongest thing in the project.]

## Latency

[histogram plot, LOG-SCALE Y-AXIS, tail clearly visible]

| p50 | p99 | p99.9 | p99.99 | max |
|-----|-----|-------|--------|-----|
| Xns | Xns | Xns   | Xns    | Xns |

Latency is FLAT from 1k to 100k resting orders: [plot]

## How it works
[flat array + bitmap + arena — the 3-paragraph version]

## Determinism
Same log → same output → same state hash. 100 runs, byte-identical.
Production bug? Ship me the log. I'll replay it exactly on my laptop.

## perf
[perf stat output — cache misses per order]
Zero allocations in steady state (proven with a panicking global allocator).

## Things I tried that didn't work
[← this section is a STRENGTH, not a weakness]

## Limitations
[honest: single symbol, no market data feed, Modify loses priority, etc.]
```

---

## Anti-patterns — if you find yourself doing these, STOP

| ❌ Don't | ✅ Do |
|---|---|
| `HashMap<Price, VecDeque<Order>>` | Flat array + bitmap + arena |
| `f64` for price | `i64` ticks |
| `Instant::now()` in the engine | `Seq` from the input event |
| `Box<Order>` | `u32` arena index |
| `Vec::push` in the hot path | Pre-allocated arena, free-list |
| `.unwrap()` on input | Emit `Rejected` |
| Optimizing before Phase 5 | Correct → measure → optimize |
| Optimizing the reference engine | Leave it slow and obvious |
| Writing the fast engine first | Reference engine FIRST — it's the spec |
| Benchmarking with `random_order()` | Agent-based simulator |
| Leading the README with throughput | Lead with the bug list |
| Hiding failed optimizations | Publish them — it's a strength |
| Hashing arena indices in `state_hash` | Hash semantics, not layout |
| Freeing a slot before reading `next` | Cache `next` FIRST |
| Fill at the aggressor's price | Fill at the **RESTING** order's price |

---

## When stuck

- **Fast and reference disagree?** The fast engine is guilty until proven innocent. But check the reference too — sometimes the oracle is wrong, and that's a great README story.
- **Fuzzer finds nothing?** Your generator is too tame. Add adversarial weight: same-trader, boundary prices, exact-fill quantities, ID collisions.
- **Nondeterminism?** You're hashing arena indices, or something in `core` is reading a clock/RNG. Grep again.
- **Latency tail is bad?** Frequency scaling, an unpinned thread, or an allocation you didn't know about. Check all three before you touch the algorithm.
- **Optimization made it slower?** **Good.** Write it up. That's a README section, not a failure.
- **Scope creep?** Multi-symbol, market data feeds, FIX protocol, and networking are **all out of scope.** One symbol, in-memory, done well.

---

## Final checklist

- [ ] Zero floats in `core/` (grep `f32`, `f64` → 0 hits)
- [ ] Zero clock/RNG in `core/` (grep `Instant`, `SystemTime`, `rand` → 0 hits)
- [ ] `core` is `#![no_std]` and compiles
- [ ] Zero allocations in steady state (proven with the panicking allocator)
- [ ] Fast and reference engines agree over 8+ CPU-hours of fuzzing
- [ ] Every fuzzer-found bug has a named regression test
- [ ] `docs/BUGS.md` lists every bug with root cause and repro
- [ ] Determinism: 1M-event log × 100 runs → identical state hash
- [ ] Latency histogram in the README, log-scale y-axis
- [ ] Latency is flat from 1k → 100k resting orders
- [ ] `perf stat` output in the README, sub-1 cache miss per order
- [ ] A "things that didn't work" section exists and is honest
- [ ] A 30-second demo (GIF/asciinema) exists
- [ ] `cargo clippy -- -D warnings` is clean
- [ ] Someone can clone → one command → live book in under 60 seconds
