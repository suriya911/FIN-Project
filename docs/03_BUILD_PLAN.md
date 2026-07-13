# Tessera — Build Plan

~3 weeks of real evenings-and-weekends work. **Do not let it sprawl past that.** A finished, well-documented 3-week project beats an abandoned 3-month one, every time.

Phases are ordered by dependency. Do not skip ahead — in particular, **do not optimize before Phase 5**, and **do not skimp on Phase 3**, which is the actual project.

---

## Phase 0 — Foundation
**1–2 days**

Get the types and the oracle in place. Write the *slow* engine first.

- [ ] `cargo new --lib tessera`, workspace with crates: `core`, `sim`, `fuzz`, `shell`
- [ ] Define `Price`, `Qty`, `OrderId`, `TraderId`, `Seq`, `Side`, `TimeInForce`, `SelfTradePrevention`
- [ ] Define `InputEvent`, `OutputEvent`, `CancelReason`, `RejectReason`
- [ ] **Write the reference engine.** `Vec<Order>`, sorted, linear scan. ~200 lines. Slow and obvious.
- [ ] Hand-write 20 unit tests against the reference: simple cross, partial fill, multi-level sweep, cancel, IOC, FOK, self-trade
- [ ] CI: `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`

**Exit criteria:** the reference engine passes all 20 tests. You now have an oracle.

> **Why the reference first?** Because it's the specification. If you write the fast engine first, you'll unconsciously write the reference to agree with it, and the differential fuzzer becomes worthless. Write the definition of correct *before* you write the thing being tested.

---

## Phase 1 — Fast engine
**3–5 days**

Structurally correct. **Not yet optimized.**

- [ ] `Arena` with `OrderSlot`, free-list, `with_capacity`, `alloc`, `free`
- [ ] `BookSide`: flat `levels: Vec<Level>` array, `occupied` bitmap, cached `best`
- [ ] `price_to_idx` / `idx_to_price` with bounds checks → `PriceOutOfBounds`, never panic
- [ ] `order_index`: direct-mapped or flat open-addressed map. **Not `std::HashMap`.**
- [ ] Intrusive FIFO: `link_tail`, `unlink_and_free`
- [ ] `rescan_best_bid` / `rescan_best_ask` (bitmap scan, `leading_zeros` / `trailing_zeros`)
- [ ] The match loop: cross → fill levels → rest / IOC-kill
- [ ] FOK pre-check (walk and sum, **do not mutate**)
- [ ] Self-trade prevention: all four modes
- [ ] `Modify` = cancel + new (loses priority — document it)
- [ ] `validate()` checking invariants I1–I10, `#[cfg(debug_assertions)]`-gated
- [ ] **Same 20 tests from Phase 0 now pass against the fast engine**

**Exit criteria:** fast engine passes the same test suite as the reference. `validate()` passes after every event.

> **Resist optimizing here.** You will be tempted. Don't. You have no profiler data yet, so any "optimization" is a guess, and guessing is how you end up with a fast, wrong engine. Get it correct, then measure, then optimize.

---

## Phase 2 — Determinism
**1–2 days**

Make the core a pure state machine, and prove it.

- [ ] Make the `core` crate `#![no_std]`. It now *cannot* call the clock or an RNG.
- [ ] Grep the core for `Instant`, `SystemTime`, `rand`, `thread`, `Box`, `HashMap` → all must be gone
- [ ] `state_hash()` — canonical order (levels ascending, FIFO head→tail). **Hash semantics, not arena indices.**
- [ ] Journal format: append-only binary log of `InputEvent`
- [ ] `replay(log) -> (Vec<OutputEvent>, u64)` harness
- [ ] Snapshot / restore: serialize book state, restore, fast-forward from event N
- [ ] Determinism test: run the same 1M-event log 100× → identical output *and* identical state hash
- [ ] Cross-machine check: same log on two different machines / OSes → same hash

**Exit criteria:** `replay_is_deterministic` passes 100 iterations. Snapshot-at-N + replay-from-N == full replay.

> This phase is short but it unblocks everything. The differential fuzzer is **impossible** if either engine is nondeterministic — you'd never be able to tell a real bug from a flaky one.

---

## Phase 3 — Correctness ★ THIS IS THE PROJECT ★
**3–4 days. Do not rush it.**

Everything else is table stakes. *This* is what gets you the interview.

- [ ] `proptest` generators: valid random `InputEvent` streams (weighted: ~10% new, ~85% cancel, ~5% modify — realistic ratios)
- [ ] `proptest` invariant suite: assert I1–I10 after **every** event
- [ ] Property: conservation — `Σ fills + Σ remaining + Σ cancelled == Σ submitted`
- [ ] Property: book never crosses
- [ ] Property: FIFO priority is respected (older order at the same price always fills first)
- [ ] `cargo-fuzz` target: **differential harness**
  - structured input → both engines → compare `OutputEvent` streams
  - divergence → shrink → panic with the minimal reproducer
- [ ] Write the 15 known-hard cases from the design doc as explicit tests
- [ ] **Run the fuzzer overnight** (`cargo fuzz run differential -- -max_total_time=28800`)
- [ ] Fix every divergence. **Commit each as a named regression test** in `tests/regressions/`
- [ ] Keep a running bug log: what broke, why, what the minimal repro was

**Exit criteria:** 8+ CPU-hours of fuzzing with zero divergence. A bug list with at least a handful of real, interesting bugs — and their reproducers committed.

> **The bug list is the deliverable.** *"Differential fuzzing found 14 bugs in my own engine. Here they are, here are the minimal reproducers, here's what each one taught me."* No other candidate will have this. It's the whole pitch.
>
> If the fuzzer finds nothing in 8 hours, your generator is too tame. Crank up the adversarial weight: same-trader orders, boundary prices, exact-fill quantities, cancel-immediately-after-fill, IDs that collide mod capacity.

---

## Phase 4 — Simulator
**2–3 days**

Realistic load, so the benchmark means something.

- [ ] `Agent` trait: `fn act(&mut self, book_view: &BookView, rng: &mut Rng) -> Vec<InputEvent>`
- [ ] Seeded PRNG (xorshift / PCG). **No `thread_rng`.** `--seed 42` → identical market, always.
- [ ] **MarketMaker**: quotes both sides around a reference price; cancels + requotes on drift. Generates the realistic ~90% cancel rate.
- [ ] **Momentum**: crosses the spread on directional flow.
- [ ] **Noise**: Poisson-arrival random orders.
- [ ] **Adversarial**: hammers one level; submit-cancel tight loops; orders at book edges; exact-fill quantities.
- [ ] Reference price process (geometric Brownian motion or a simple random walk)
- [ ] Sim runner: `--agents 100 --seed 42 --events 10M --out log.bin`
- [ ] Stats output: order-to-trade ratio, cancel rate, mean book depth, spread distribution — **sanity-check these against real market microstructure numbers**

**Exit criteria:** the simulator produces a log with a ~90% cancel rate and a realistically-shaped book. Same seed → byte-identical log.

> This is also your best fuzzer input generator. Feed simulator logs into the differential harness — realistic sequences find different bugs than uniformly-random ones.

---

## Phase 5 — Performance
**3–5 days**

**Now** you optimize. Guided by `perf`, not by vibes.

- [ ] `criterion` bench harness: throughput + per-op latency
- [ ] `hdrhistogram` for latency: p50 / p99 / p99.9 / p99.99 / max
- [ ] **Baseline first.** Record the numbers before touching anything.
- [ ] `perf stat -e cycles,instructions,cache-misses,cache-references,branch-misses`
- [ ] `flamegraph` → find the actual hot path (it will surprise you)
- [ ] Pin to an isolated core (`taskset`), disable frequency scaling, `nice -20`
- [ ] Enable the panicking allocator → **prove zero allocation in steady state**

Then, one change at a time, measuring each:

- [ ] Reorder `OrderSlot` fields → hot fields in the first cache line
- [ ] `#[inline(always)]` on `price_to_idx`, `crosses`, the FIFO link/unlink
- [ ] Prefetch the next order in a level walk (`_mm_prefetch` on the cached `next`)
- [ ] Avoid the bitmap rescan when the best level doesn't empty
- [ ] Branch hints on the rare paths (reject, arena-full, STP)
- [ ] Try: level walk without bounds checks (`get_unchecked`, gated behind a feature + a fuzzing proof)

Then benchmark the things that actually matter:

- [ ] **Latency vs. book depth** (1k / 10k / 100k resting orders — it should stay FLAT)
- [ ] **Latency under cancel storm** (90% cancel rate)
- [ ] **Latency under adversarial load** (all orders at one level)
- [ ] Cold-start vs. warm (report honestly)
- [ ] Throughput (orders/sec) — the headline. Report it last.

**Exit criteria:** a latency histogram plot committed to the README. Sub-1 cache miss per order. Zero allocations proven. **And a written record of every optimization you tried — including the ones that failed.**

> **Write down what didn't work.** *"I tried prefetching the next level's head pointer and it was 3% slower because it evicted the current level's FIFO from L1. Here's the `perf` output."*
>
> That paragraph is worth more in an interview than a list of tricks that happened to work. It proves you measure instead of cargo-cult.

---

## Phase 6 — Shell & polish
**2–3 days**

Make it demo-able in 30 seconds.

- [ ] SPSC ring buffer ingress/egress (`crossbeam` or hand-rolled)
- [ ] Simple binary wire protocol: encode / decode `InputEvent` and `OutputEvent`
- [ ] Journal writer (append-only input log → enables replay from a real session)
- [ ] **TUI live book view** (`ratatui`) — depth ladder, last trades, live latency histogram. *This is what makes it demo-able.*
- [ ] CLI: `tessera sim --seed 42 --agents 100`, `tessera replay log.bin`, `tessera bench`
- [ ] README:
  - [ ] The one-line pitch
  - [ ] Architecture diagram
  - [ ] **Latency histogram plot (log-scale y-axis, visible tail)**
  - [ ] `perf stat` output showing cache misses
  - [ ] **The bug list from differential fuzzing** ← lead with this
  - [ ] Design rationale: why flat array, why bitmap, why arena, why no floats
  - [ ] **"Things I tried that didn't work"**
  - [ ] Honest limitations section
- [ ] A 30-second asciinema / GIF of the TUI under simulated load

**Exit criteria:** someone can clone it, run one command, and see a live order book with a latency histogram inside 60 seconds.

---

## Timeline

| Phase | Days | Cumulative |
|---|---|---|
| 0 — Foundation | 1–2 | 2 |
| 1 — Fast engine | 3–5 | 7 |
| 2 — Determinism | 1–2 | 9 |
| **3 — Correctness ★** | **3–4** | **13** |
| 4 — Simulator | 2–3 | 16 |
| 5 — Performance | 3–5 | 21 |
| 6 — Shell & polish | 2–3 | 24 |

**~3 weeks.** Hard stop. Ship it.

---

## If you're short on time

Cut in this order:

1. **Cut the TUI.** A static plot in the README works. (Costs you the demo wow-factor, but the substance survives.)
2. **Cut the shell / wire protocol.** Run the engine straight from a log file.
3. **Cut the adversarial agent.** Keep MM + noise.
4. **Cut Modify.** Support New + Cancel only. Document it as out of scope.

**Never cut:** Phase 2 (determinism) or Phase 3 (differential fuzzing). Without those you've built the same order book as every other candidate, and the entire differentiator is gone.

---

## Interview talking points this earns you

Build it and you can answer, from experience:

- *"Why not a `HashMap` for price levels?"* → cache behavior, hashing cost, allocator in the hot path. And you have the `perf` numbers.
- *"How do you find the best price?"* → bitmap + `leading_zeros`, cached best, rescan only on level-empty.
- *"How do you know it's correct?"* → differential fuzzing against a reference. Here are the 14 bugs it found.
- *"What's your p99.99?"* → you have the histogram. You know the tail. You know *why* the tail looks like that.
- *"How would you debug a production issue?"* → ship me the log, I'll replay it bit-for-bit on my laptop.
- *"What's the worst case?"* → the adversarial agent. Here's the latency under it.
- *"What would you do differently?"* → you have an honest answer, because you kept the failed-optimization log.
