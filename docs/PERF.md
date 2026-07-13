# Performance results

## Measurement environment — read this first

All numbers below were measured in a **shared cloud container** (4 vCPUs,
Linux 6.18, Rust 1.94.1, `--release` with LTO + `codegen-units = 1`):

- no isolated cores (`isolcpus`), no frequency-scaling control,
- `perf` hardware counters unavailable (no cache-miss / branch-miss data),
- timing via `Instant::now()` (~20–30 ns overhead per sample, which
  inflates the LOW end of every distribution),
- noisy neighbors: **the p99.99 and max columns largely measure the
  hypervisor, not the engine.** On pinned bare metal the tail collapses;
  the p50–p99.9 columns are the ones this environment measures credibly.

Every number is reproducible: `cargo bench -p tessera-bench` and
`cargo run --release -p tessera-bench --bin noalloc_proof`.

## Latency, realistic agent workload (2M events, seed 42)

| workload | p50 | p99 | p99.9 | p99.99 | max |
|---|---|---|---|---|---|
| realistic mix (92% cancel rate) | 59 ns | 262 ns | 610 ns | 21 µs\* | 343 µs\* |
| cancel storm (90% cancels, 50k resting) | 84 ns | 308 ns | 542 ns | 25 µs\* | 97 µs\* |

\* container-noise dominated; see environment note.

## Latency vs. book depth — THE design claim

Identical touch-level workload against books pre-filled to three sizes:

| resting orders | p50 | p99 | p99.9 |
|---|---|---|---|
| 1,000 | 44 ns | 146 ns | 393 ns |
| 10,000 | 45 ns | 151 ns | 404 ns |
| 100,000 | 45 ns | 192 ns | 365 ns |

**Flat.** Two orders of magnitude more resting orders, same latency.
That is the whole point of flat-array levels + occupancy bitmap + cached
best: the common path never traverses a structure whose size depends on
the book.

## Zero allocations in steady state — proven

`noalloc_proof` builds the book, buffer, and a 2M-event realistic log,
then arms a global allocator that panics on ANY allocation, then applies
all 2M events:

```
PROOF OK: 2000000 events applied with the panicking allocator armed —
zero allocations in steady state (1659 orders still live).
```

## Throughput (the vanity metric, reported last)

criterion, replaying 1M realistic agent events into a fresh book:

```
engine/replay_1M_realistic_events
    time:  [71.9 ms  75.5 ms  80.9 ms]
    thrpt: [12.4 M/s 13.2 M/s 13.9 M/s]
```

~**13M events/second** single-threaded, ~75 ns/event mean, consistent
with the histogram p50.

## Things I tried that didn't work

### Prefetching the next FIFO slot during level walks

Theory: `fill_at_level` caches `next` before touching the current maker,
so issue `_mm_prefetch(T0)` on the next slot and hide its load latency.

Result — it was **slower**:

| cancel storm | p50 | p99 |
|---|---|---|
| baseline (3 runs) | 84 / 85 / 84 ns | 308 / 296 / 320 ns |
| with prefetch (3 runs) | 113 / 90 / 87 ns | 435 / 373 / 349 ns |

Why: the arena free-list recycles hot slots, so in cancel-heavy flow the
"next" slot is almost always already in L1; the prefetch is pure extra
work (plus a NIL branch) on every iteration of the hottest loop. Reverted
(the experiment lives in this table, not in the code). No `perf` counters
in this container to confirm the L1-residency theory directly — flagged
for re-testing on bare metal.

### Deliberately NOT attempted here

`get_unchecked` in the level walk, branch hints, and OrderSlot field
reordering experiments need hardware counters to evaluate honestly (the
field order was set from the design doc's cache-line analysis at
implementation time). Rule 20 — no perf data means an optimization is a
guess — cuts both ways: without counters this environment can't prove a
win isn't noise, so those stay on the bare-metal to-do list.
