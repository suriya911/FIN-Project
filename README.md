# Tessera

**A deterministic limit order book that can prove it's correct.**

Price-time priority matching engine in Rust. `#![no_std]` core with zero
dependencies, zero allocation in the hot path, zero clocks, zero RNG.
Raced against a deliberately-slow reference oracle by a differential
fuzzer, hammered by an agent-based market simulator, and replayable
bit-for-bit from an append-only journal.

```
cargo run --release -p tessera-shell --bin tessera -- tui
```

One command → a live order book: depth ladders, last trades, rolling
latency, ~90%-cancel-rate agent flow. `q` quits.

---

## Correctness is the product. Here is the evidence.

Every behavior is defined twice: a fast engine (arena + flat levels +
bitmap) and a ~380-line reference oracle (sorted `Vec`, linear scans,
obviously correct by inspection), written **first**, as the executable
spec. They must produce **byte-identical output streams** and identical
canonical state hashes, event by event, forever.

| Harness | Volume | Result |
|---|---|---|
| `difftest` — seeded streams, 3 book shapes | 14,283 seeds × 20k events × 3 configs ≈ **857M events** | zero divergences |
| `cargo fuzz` (libFuzzer) differential target | 2.7M structured inputs | zero divergences |
| proptest — 6 properties, shrinking | 512 cases/property/run in CI | pass |
| invariant validator (I1–I9) after **every** event in every harness | — | zero violations |
| 1M-event log × 100 replays (release) | 100M events | byte-identical outputs + state hash |

**Zero divergences found in the finished engine.** A fuzzer that finds
nothing could just be blind — so the classic order-book bugs were
deliberately injected and the harness had to catch each one
([docs/BUGS.md](docs/BUGS.md) has the full table):

| Injected bug | Caught |
|---|---|
| Fill emitted at the wrong price | event **1**, seed 0 |
| Bitmap bit not cleared when a level empties | event **6**, seed 0 (invariant I3) |
| FOK pre-check ignoring self-trade prevention | event 2,872, seed 0 |
| Fully-filled maker left as a zero-qty ghost | seconds — livelock flagged by invariant I9 |

The bugs that *were* found during development (two spec conflicts, a
quantity-overflow risk, an i8-overflow bug in the test generator itself,
and more) are logged with root causes in
[docs/BUGS.md](docs/BUGS.md).

## Latency

Realistic agent workload (92% cancel rate), 2M events, release build:

| p50 | p99 | p99.9 | p99.99\* | max\* |
|---|---|---|---|---|
| 58 ns | 250 ns | 518 ns | 9.0 µs | 275 µs |

```
  ns bucket      count  log-scale (each # ≈ one power of ten)
         16+      11732  #####
         32+    1120503  #######
         64+     762916  ######
        128+      85452  #####
        256+      17327  #####
        512+       1184  ####
       1024+        421  ###
       2048+        152  ###
       4096+        186  ###
       8192+         12  ##
      16384+        151  ###
      32768+         36  ##
     131072+          1  #
     262144+          1  #
```

\* Measured in a **shared cloud container** — no isolated core, no
frequency-scaling control, no `perf` counters. The p99.99/max columns
largely measure the hypervisor, not the engine; see
[docs/PERF.md](docs/PERF.md) for the full methodology and caveats.

**Latency is FLAT from 1k to 100k resting orders** — the design claim
that motivates the whole data-structure choice:

| resting orders | p50 | p99 | p99.9 |
|---|---|---|---|
| 1,000 | 44 ns | 146 ns | 393 ns |
| 10,000 | 45 ns | 151 ns | 404 ns |
| 100,000 | 45 ns | 192 ns | 365 ns |

Cancel storm (90% cancels against 50k resting): p50 84 ns, p99 308 ns.

**Zero allocations in steady state — proven, not claimed:** a global
allocator that panics while armed ran 2M events through `apply()`
without firing (`cargo run --release -p tessera-bench --bin
noalloc_proof`).

## How it works

Three structures, no cleverness anywhere else:

**Flat level array.** A price maps to a level index by
`(price − min) / tick` — one subtract and one divide, no hashing, no
tree, no pointer chase. Each level is `{head, tail, total_qty, count}`.

**Occupancy bitmap + cached best.** One bit per level. The best price is
a cached index, rescanned only when the best level *empties* — and the
rescan is `leading_zeros`/`trailing_zeros` over u64 words, usually one
instruction. Best-price lookup never walks anything.

**Order arena.** Every order is a 56-byte slot (hot fields in the first
cache line) in one pre-allocated slab, addressed by `u32` index —
`u32::MAX` is NIL, never a `Box` in sight. Orders at a level form an
intrusive doubly-linked FIFO through the arena, so price-time priority
falls out for free and nothing is ever sorted. Freed slots chain into a
free-list that reuses the `next` field. Cancels resolve OrderId → slot
through a pre-allocated open-addressed flat map (not `std::HashMap`) and
splice in O(1).

## Determinism

The core is a pure state machine: `(state, event) → (state', outputs)`.
It is `#![no_std]` with **zero dependencies**, so it *cannot* read a
clock, an RNG, or the OS — the type system enforces what the rules ask.
Time is the input's sequence number. CI greps the core for
`Instant|SystemTime|rand|thread|HashMap|BTreeMap|f32|f64` and fails on
any hit; the determinism suite pins a state-hash constant on both Linux
and macOS.

```
$ tessera sim --seed 42 --events 1000000 --out session.bin
final state hash 0xc098683cf7025353
$ tessera replay session.bin --repeat 10
state  hash 0xc098683cf7025353
10 replays, byte-identical outputs and state hash — deterministic.
```

Production bug? Ship the journal. It replays exactly, anywhere.
Snapshots restore and fast-forward: `snapshot(N) + replay_from(N)`
equals a full replay, hash-verified in the test suite.

State hashes cover *semantics* (orders in canonical level/FIFO order),
never arena indices — physical slot placement is free to differ. Both
engines implement the hash independently, so the hash itself is
differentially tested.

## The simulator (why the benchmark means something)

`random_order()` in a loop produces a fantasy book. Tessera's load comes
from agents: market makers that cancel-and-requote around a GBM
reference price (the source of the real-world ~90% cancel rate),
momentum flow that sweeps levels, Poisson noise, and an adversarial
agent that hammers one level, spams unfillable FOKs, and probes exact
fills at the touch. Measured at 1M events: **92.3% cancel rate**, mean
depth ~2,400 resting orders, 1.5-tick spread. Same seed → byte-identical
market, every run.

## Things I tried that didn't work

**Prefetching the next FIFO slot in the level walk.** The walk caches
`next` before touching the current maker, so prefetching it *should*
hide the load. It was **slower**: cancel-storm p50 went 84 ns → 87–113 ns,
p99 ~300 ns → 350–435 ns. The arena free-list recycles hot slots, so the
next slot is almost always in L1 already; the prefetch is pure overhead
in the hottest loop. Reverted — numbers and reasoning in
[docs/PERF.md](docs/PERF.md). Optimizations that need hardware counters
to evaluate honestly (`get_unchecked`, branch hints, field-order
shuffles) are parked until this runs on bare metal, per the project's
own rule: no profiler data, no optimization.

## Repository map

| | |
|---|---|
| `crates/core` | the engine: `no_std`, zero deps, arena/levels/bitmap/index, match loop, invariant validator, canonical hash, snapshots |
| `crates/reference` | the oracle: slow, obvious, written first, never optimized |
| `crates/sim` | seeded PCG32, agents, steppable runner, stats |
| `crates/shell` | codec, journal, SPSC ring, ratatui TUI, `tessera` CLI |
| `crates/tests` | one behavioral suite run against BOTH engines, properties, hard cases, determinism, `difftest` |
| `crates/fuzz` | the libFuzzer differential target |
| `benches/` | hdrhistogram latency, depth scaling, cancel storm, criterion throughput, `noalloc_proof` |
| `docs/` | design docs, [BUGS.md](docs/BUGS.md), [PERF.md](docs/PERF.md) |

```
cargo test --workspace                  # everything, both engines
cargo run --release -p tessera-tests --bin difftest -- --minutes 60
cargo +nightly fuzz run differential --fuzz-dir crates/fuzz
cargo bench -p tessera-bench
```

## Limitations (honest)

- **Single symbol, in-memory.** Multi-symbol, market data feeds, FIX,
  and networking are deliberately out of scope.
- **No market orders.** The spec's sentinel-price market orders conflict
  with grid validation (BUGS.md/BUG-001); a boundary-price IOC is the
  supported equivalent.
- **`Modify` = cancel + new and always loses time priority** — a
  documented design decision, not an accident.
- **Throughput** (reported last, as it deserves): ~13M events/s
  single-threaded replaying realistic agent flow.
- Latency tails above p99.9 are unmeasurable in this environment;
  `perf stat` cache-miss counts and a bare-metal histogram are the
  remaining to-dos, flagged in [docs/PERF.md](docs/PERF.md).
- No demo GIF yet — the TUI is one command away (top of this file).
