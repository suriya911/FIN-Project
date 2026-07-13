# Tessera — Project Overview & Architecture

A deterministic, replayable limit order book matching engine with an agent-based market simulator and a differential fuzzer.

---

## The honest framing

The matching algorithm is 60 years old and solved. Price-time priority is not where innovation lives, and any quant firm interviewing you knows that. Every candidate who builds "a matching engine" builds the same `HashMap<Price, VecDeque<Order>>` and reports a throughput number nobody can verify.

**The differentiator is not the matcher. It is everything around it.**

This project is built on one claim:

> *I built a matching engine that can prove it's correct, not just claim it.*

That claim is backed by three things almost nobody bothers to build:

1. **Bit-for-bit determinism.** Same input log → same output log → same state hash, every time. This is what real exchanges require and what makes production bugs reproducible on a laptop.
2. **A differential fuzzer.** A second, deliberately-slow, obviously-correct reference engine runs alongside the fast one. Any divergence is a bug. Run it for CPU-hours and it *will* find things.
3. **An agent-based simulator.** Realistic load — 90% cancel rates, real order-to-trade ratios, real book depth — instead of `for i in 0..10M { random_order() }`, which produces a meaningless benchmark against an unrealistic book.

"Found 14 real bugs in my own engine via differential fuzzing, here are the minimal reproducers" is a stronger interview signal than any orders/sec number.

---

## Three components

```
┌─────────────────────────────────────────────────────────┐
│  SIMULATOR (agents)                                     │
│  market makers · momentum · noise · adversarial         │
│  seeded PRNG → deterministic synthetic market           │
└────────────────────────┬────────────────────────────────┘
                         │ InputEvent stream
                         ▼
┌─────────────────────────────────────────────────────────┐
│  ENGINE CORE  (pure state machine, no_std)              │
│                                                         │
│  (BookState, InputEvent) → (BookState', [OutputEvent])  │
│                                                         │
│  no clocks · no RNG · no syscalls · no threads          │
│  no allocation · no floats · no HashMap in hot path     │
└────────────────────────┬────────────────────────────────┘
                         │ OutputEvent stream
                         ▼
┌─────────────────────────────────────────────────────────┐
│  DIFFERENTIAL FUZZER                                    │
│  same inputs → reference engine (slow, obvious)         │
│  compare outputs → diverge? shrink → regression test    │
└─────────────────────────────────────────────────────────┘
```

The engine core is a **functional core, imperative shell**. I/O, networking, and time live entirely outside the matcher. This is not a stylistic preference — it is the constraint that makes determinism possible, and everything else follows from it.

---

## How the book works

### The naive version (what everyone builds)

```rust
HashMap<Price, VecDeque<Order>>  // + a BTreeMap for sorted price levels
```

It works. It is slow. Every operation hashes, then chases pointers into random cache lines. Cancel is O(n) unless you bolt on another index. The allocator is in your hot path.

### The fast version

```
Price levels   → a FLAT ARRAY, indexed by (price - min_price) / tick_size
                 O(1) lookup. No hashing. No tree. No pointer chasing.

Best bid/ask   → a BITMAP (u64 words) over occupied levels
                 find best price = one trailing_zeros() / leading_zeros() instruction

Orders         → an ARENA. Order = a u32 index into a Vec, never a Box.
                 Intrusive doubly-linked list per level; next/prev are u32 indices.

Cancel         → O(1). order_id → arena index via a flat map, unlink in place.
```

Every order lives in a contiguous arena. Every price level is a slot in a flat array. There are **no allocations in the hot path, no pointer chasing, and no branches on data you didn't just touch.**

### The bitmap trick

This is the good part. To find the best bid you do not walk a tree — you scan a `[u64; N]` bitmap for the highest set bit.

On a book with 65,536 price levels that's 1,024 words. But in practice you **cache the best-price index** and only rescan when a level empties. Finding the new best price after a level clears is a `leading_zeros()` on a single word, most of the time.

Best-price lookup collapses from a tree traversal into a single CPU instruction.

### The match loop

```
Order arrives → validate → does it cross?
   │
   ├─ CROSS:  while remaining > 0 && crosses best opposite price:
   │             walk the FIFO at that level, fill OLDEST FIRST
   │             emit Trade events
   │             pop filled orders back to the arena free-list
   │             clear the bitmap bit if the level empties
   │             advance to the next price level
   │
   └─ REST:   if remaining > 0:
                 append to arena
                 link into the level's FIFO tail
                 set the bitmap bit
                 update the best-price cache

Emit → Ack / Fill / PartialFill / Cancelled / Rejected → output ring buffer
```

**Price-time priority falls out for free.** The flat array gives you price ordering. The per-level FIFO gives you time ordering. You never sort anything.

---

## How determinism works

The engine is a **pure state machine**:

```
(BookState, InputEvent) → (BookState', [OutputEvent])
```

- **No clocks.** Timestamps arrive *inside* the input event. The engine never calls `Instant::now()`.
- **No RNG.**
- **No syscalls.**
- **No threads inside the core.**
- **No allocation.**

### What this buys you

| Property | Consequence |
|---|---|
| Same log twice → identical output | Byte for byte. Verifiable with a hash. |
| Production bug → laptop repro | Ship the log, replay it, watch it break. Exactly. |
| State hash after every event | Two runs diverge? You know the exact event index. |
| Snapshot + fast-forward | Crash recovery is trivial. Snapshot at event N, restore, replay from N. |

This is how real exchanges are built, and it is the single most defensible thing in the project. It is also the thing that makes the differential fuzzer possible at all — you cannot compare two engines if either of them is nondeterministic.

---

## How the simulator works

Benchmarking with `for i in 0..10_000_000 { random_order() }` produces an unrealistic book and a **meaningless** number. Real books have ~90% cancel rates, deep resting liquidity, and bursty directional flow.

So instead: **agents.**

| Agent | Behavior | What it stresses |
|---|---|---|
| **Market maker** | Quotes both sides around a reference price. Cancels and requotes as it drifts. | The cancel path. Generates the realistic ~90% cancel rate. |
| **Momentum** | Crosses the spread on directional flow. | The match loop, multi-level sweeps. |
| **Noise** | Poisson-distributed random orders. | Baseline load. |
| **Adversarial** | Hammers one price level. Submit-cancel in tight loops. Orders at book edges. | Worst-case paths. Tries to break you. |

All agents use **seeded PRNGs**. `--seed 42` always produces the same market. The simulation is as deterministic as the engine.

This gives you a benchmark that *means something*, and a load generator that can actually find your worst case.

---

## How the differential fuzzer works

Write a second matching engine. Deliberately, insultingly slow: a `Vec<Order>`, sorted, linear scan, allocate everything, ~200 lines. **Obviously correct by inspection.**

Then race them.

```
    structured random operation stream (cargo-fuzz)
                    │
        ┌───────────┴───────────┐
        ▼                       ▼
   FAST ENGINE            REFERENCE ENGINE
        │                       │
   output events           output events
        └───────────┬───────────┘
                    ▼
              compare
                    │
            diverge? ──► SHRINK to minimal case ──► crash loudly
                    │
                    └──► commit as a named regression test
```

Any divergence is a bug in the fast engine — or, occasionally, a delightful bug in the reference.

**Bugs this reliably finds:**
- Self-trade prevention edge cases
- An order that fills exactly to zero and never gets unlinked
- A price level whose bitmap bit doesn't clear when it empties
- Integer overflow on absurdly large quantities
- Modify-in-place that silently loses time priority

Run it overnight. Fix what it finds. Commit every bug as a named regression test. **Put the bug list in the README.**

---

## Tech stack

**Language: Rust.**

Not because it's trendy. Because for *this specific project* it gives you arena/index-based data structures without undefined behavior, it makes the "zero allocation in hot path" claim mechanically checkable, and `cargo-fuzz` + `proptest` are exactly the tools this design needs.

C++ is a completely fine alternative if you're already strong in it — quant firms are C++ shops and will not hold Rust against you. They *will* hold sloppy C++ against you.

### Used

| Layer | Choice | Why |
|---|---|---|
| Engine core | `no_std`, zero deps | Nothing in the hot path you didn't write |
| Money | Fixed-point `i64` ticks | **Never floats.** Ever. |
| Orders | Arena + `u32` indices | Contiguous, cache-friendly, no `Box` |
| Levels | Flat array + bitmap | O(1) lookup, 1-instruction best-price |
| Benchmarking | `criterion` | Statistical rigor, not a stopwatch |
| Latency | `hdrhistogram` | A mean latency number is a lie |
| Properties | `proptest` | Book never crosses. No order is ever lost. |
| Fuzzing | `cargo-fuzz` / libFuzzer | The differential harness |
| Profiling | `perf` + `flamegraph` | Cache misses, branch misses |
| Shell I/O | `crossbeam` SPSC ring | LMAX-Disruptor-style, outside the core |

### Deliberately NOT used

`async` · `tokio` · locks · `Box` · `HashMap` in the hot path · floats · allocation after startup · threads inside the engine

Every one of these is a decision you should be able to defend out loud in an interview.

---

## Benchmarks that matter

Throughput is the vanity metric. Publish it — but do not *lead* with it.

| Metric | Why an interviewer cares |
|---|---|
| **p50 / p99 / p99.9 / p99.99 latency (ns)** | Trading firms live in the tail. **p99.99 is the number they'll ask about.** |
| **Latency under cancel storm** | 90% cancel rate is real. Does the book degrade? |
| **Latency vs. book depth** | Flat at 100k resting orders? It should be — that's the whole point of the design. |
| **Cache misses per order** | `perf stat -e cache-misses`. Sub-1 is the flex. |
| **Allocations in steady state** | Zero. Prove it with a global allocator that panics. |
| **Cold-start vs. warm** | Honest reporting builds credibility. |
| Throughput (orders/sec) | The headline. Fine. Put it last. |

**A latency histogram plot with a log-scale y-axis and a visible tail is worth more than any throughput claim.**

---

## What "done" looks like

- [ ] `cargo bench` produces a latency histogram, committed to the README
- [ ] `cargo fuzz run differential` runs for hours without divergence
- [ ] A bug list in the README: *"differential fuzzing found N bugs; reproducers in `tests/regressions/`"*
- [ ] Replay determinism test: same log × 100 runs → identical state hashes
- [ ] `perf stat` output in the README showing sub-1 cache miss per order
- [ ] A 30-second demo: TUI showing the live book under simulated load
- [ ] A "things I tried that didn't work" section — **this is a strong signal, not a weak one**
