# Tessera — Technical Design Specification

Concrete data structures, algorithms, and invariants. This is the document you implement from.

---

## 1. Core types

Money is **never** a float. Prices are integer ticks. Quantities are integer lots.

```rust
/// Price in ticks. tick_size is a book-level constant.
/// Real price = price_ticks * tick_size. Never materialize a float.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Price(pub i64);

/// Quantity in lots.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Qty(pub u64);

/// Globally unique, monotonically increasing, assigned by the SHELL, not the engine.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct OrderId(pub u64);

/// Participant identity. Used for self-trade prevention.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct TraderId(pub u32);

/// Sequence number of the input event. The engine's ONLY notion of time.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Seq(pub u64);

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Side { Bid, Ask }

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum TimeInForce {
    Gtc,  // Good-till-cancel: rest the remainder
    Ioc,  // Immediate-or-cancel: fill what you can, kill the rest
    Fok,  // Fill-or-kill: fill entirely or reject entirely
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum SelfTradePrevention {
    None,
    CancelResting,   // kill the resting order, keep going
    CancelAggressor, // kill the incoming order
    CancelBoth,
}
```

**Why `Seq` and not a timestamp:** the engine has no clock. Ordering is defined entirely by the input sequence number. A wall-clock timestamp may ride along *inside* the event for downstream consumers, but the matcher must never read it for any decision.

---

## 2. Events

The engine's entire API surface. One function: `apply(&mut self, InputEvent) -> impl Iterator<Item = OutputEvent>`.

```rust
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum InputEvent {
    New {
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        side: Side,
        price: Price,       // for a market order, use Price(i64::MAX/MIN)
        qty: Qty,
        tif: TimeInForce,
        stp: SelfTradePrevention,
    },
    Cancel {
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,   // must match the resting order's trader
    },
    /// Modify = cancel + new. ALWAYS loses time priority. This is a
    /// deliberate design choice, not a limitation — document it.
    Modify {
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        new_price: Price,
        new_qty: Qty,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum OutputEvent {
    Ack        { seq: Seq, order_id: OrderId },
    Fill       { seq: Seq, taker: OrderId, maker: OrderId,
                 price: Price, qty: Qty, taker_side: Side },
    Cancelled  { seq: Seq, order_id: OrderId, remaining: Qty, reason: CancelReason },
    Rejected   { seq: Seq, order_id: OrderId, reason: RejectReason },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum CancelReason { User, Ioc, SelfTradePrevention }

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum RejectReason {
    DuplicateOrderId,
    UnknownOrderId,
    PriceOutOfBounds,
    ZeroQty,
    FokUnfillable,
    ArenaFull,
    WrongTrader,
}
```

Every output event carries the `seq` of the input that caused it. This makes the output log self-describing and trivially diffable between two engines.

---

## 3. The arena

Orders live in one contiguous `Vec`. An order is a `u32` index, never a pointer.

```rust
/// Sentinel for "no order". u32::MAX.
const NIL: u32 = u32::MAX;

#[repr(C)]
#[derive(Copy, Clone)]
struct OrderSlot {
    // --- HOT: first cache line. Touched on every fill. ---
    remaining: u64,   // 8   mutated every partial fill
    order_id:  u64,   // 8   needed to emit the Fill event
    next:      u32,   // 4   intrusive FIFO forward link
    prev:      u32,   // 4   intrusive FIFO backward link
    price:     i64,   // 8   needed to emit the Fill event
    trader:    u32,   // 4   self-trade prevention check
    level_idx: u32,   // 4   O(1) unlink: which level am I in?
    // --- 40 bytes so far. Everything above fits in one 64B line. ---

    // --- COLD: rarely touched. ---
    original_qty: u64, // 8
    side:      u8,     // 1
    stp:       u8,     // 1
    _pad:      [u8; 6],
}
// Total: 56 bytes. Fits in one cache line with room to spare.
```

**Field order is load-bearing.** `remaining`, `next`, `order_id`, and `price` are read on every single fill. Put them first. Run `perf stat -e cache-misses` before and after reordering and you will see the difference.

```rust
pub struct Arena {
    slots: Vec<OrderSlot>,   // pre-allocated at startup, NEVER grows
    free_head: u32,          // head of the intrusive free-list
    len: usize,
}

impl Arena {
    /// Pre-allocate. This is the ONLY allocation in the engine's lifetime.
    pub fn with_capacity(n: usize) -> Self { /* chain the free-list */ }

    /// Pop from free-list. Returns None if full → emit RejectReason::ArenaFull.
    /// MUST NOT allocate.
    fn alloc(&mut self) -> Option<u32> { /* ... */ }

    /// Push back onto the free-list. Reuse `next` as the free-list link.
    fn free(&mut self, idx: u32) { /* ... */ }
}
```

The free-list reuses the `next` field. A freed slot's `next` points to the next free slot. Zero extra memory.

---

## 4. Price levels: flat array + bitmap

```rust
pub struct BookSide {
    /// Flat array of levels. Index = (price - min_price) / tick_size.
    /// Pre-allocated. Length = num_levels (e.g. 65,536).
    levels: Vec<Level>,

    /// Occupancy bitmap. Bit i set ⟺ levels[i] is non-empty.
    /// 65,536 levels → 1,024 u64 words → 8 KB. Fits in L1.
    occupied: Vec<u64>,

    /// Cached best level index. THE critical optimization.
    /// Bid: highest occupied index. Ask: lowest occupied index.
    /// NIL when the side is empty.
    best: u32,

    side: Side,
}

#[derive(Copy, Clone)]
struct Level {
    head: u32,       // arena index of the OLDEST order (fill this first)
    tail: u32,       // arena index of the NEWEST order (append here)
    total_qty: u64,  // sum of remaining. Maintained incrementally for FOK checks.
    order_count: u32,
}
```

### Price ⟷ index

```rust
#[inline(always)]
fn price_to_idx(&self, p: Price) -> Option<u32> {
    let off = p.0.checked_sub(self.min_price)?;
    if off < 0 { return None; }
    let idx = (off / self.tick_size) as u32;
    if idx >= self.levels.len() as u32 { return None; }
    Some(idx)
}
```

Out-of-bounds → `RejectReason::PriceOutOfBounds`. Never panic. Never wrap.

### Best-price lookup: the bitmap

Naive is O(num_levels). The cached `best` makes the common case O(1). You only rescan when the best level **empties**.

```rust
/// Called ONLY when the current best level becomes empty.
/// For a Bid book: find the highest set bit below `from`.
#[inline]
fn rescan_best_bid(&mut self, from: u32) {
    let mut word = (from / 64) as usize;
    let bit = from % 64;

    // Mask off bits at/above `from` in the starting word, then scan down.
    let mut w = self.occupied[word] & ((1u64 << bit) - 1);

    loop {
        if w != 0 {
            // 63 - leading_zeros = index of the highest set bit.
            self.best = (word as u32) * 64 + (63 - w.leading_zeros());
            return;
        }
        if word == 0 { self.best = NIL; return; }  // side is empty
        word -= 1;
        w = self.occupied[word];
    }
}
```

For an Ask book, mirror it: mask off bits *at or below* `from` and scan *up*, using `trailing_zeros()`.

**Why this is fast in practice:** in a real book the best level empties rarely (most flow is partial fills and cancels away from the touch). And when it does, the next occupied level is almost always in the same `u64` word — a single `leading_zeros()` instruction. The full scan is the pathological case, not the common one.

---

## 5. The order index

Cancel needs `OrderId → arena index` in O(1) with **no `HashMap`** in the hot path.

Two options. Pick based on your ID scheme.

**Option A — direct-mapped (preferred).** If the shell assigns dense, monotonic order IDs:

```rust
/// order_index[id % capacity] = arena_idx.
/// Requires the shell to guarantee no two live orders collide mod capacity.
/// Simplest, fastest, zero hashing. Document the constraint loudly.
order_index: Vec<u32>,
```

**Option B — open-addressed flat map.** If IDs are sparse or adversarial:

```rust
/// Linear-probed, power-of-two sized, pre-allocated, never grows.
/// Load factor capped at 0.5. Tombstones on delete.
/// This is NOT std::HashMap — no SipHash, no allocation, no growth.
order_index: FlatMap,
```

Do **not** use `std::collections::HashMap`. SipHash is ~20ns per lookup and the allocator is in your path.

---

## 6. The match loop

```rust
impl OrderBook {
    pub fn apply(&mut self, ev: InputEvent, out: &mut EventBuffer) {
        match ev {
            InputEvent::New { seq, order_id, trader, side, price, qty, tif, stp } => {
                // 1. VALIDATE
                if qty.0 == 0 {
                    out.push(OutputEvent::Rejected { seq, order_id,
                        reason: RejectReason::ZeroQty });
                    return;
                }
                if self.order_index.contains(order_id) {
                    out.push(OutputEvent::Rejected { seq, order_id,
                        reason: RejectReason::DuplicateOrderId });
                    return;
                }
                let Some(_) = self.side(side).price_to_idx(price) else {
                    out.push(OutputEvent::Rejected { seq, order_id,
                        reason: RejectReason::PriceOutOfBounds });
                    return;
                };

                // 2. FOK PRE-CHECK: walk levels, sum available qty, DO NOT MUTATE.
                if tif == TimeInForce::Fok && !self.can_fill_fully(side, price, qty, trader, stp) {
                    out.push(OutputEvent::Rejected { seq, order_id,
                        reason: RejectReason::FokUnfillable });
                    return;
                }

                out.push(OutputEvent::Ack { seq, order_id });

                // 3. CROSS
                let mut remaining = qty.0;
                let opp = side.opposite();

                while remaining > 0 {
                    let best_idx = self.side(opp).best;
                    if best_idx == NIL { break; }                    // book empty
                    let best_price = self.side(opp).idx_to_price(best_idx);
                    if !crosses(side, price, best_price) { break; }  // no longer marketable

                    remaining = self.fill_at_level(
                        opp, best_idx, remaining, order_id, trader, side, stp, seq, out
                    );
                }

                // 4. REST or KILL
                if remaining > 0 {
                    match tif {
                        TimeInForce::Gtc => self.rest(order_id, trader, side, price,
                                                      Qty(remaining), qty, stp, seq, out),
                        TimeInForce::Ioc => out.push(OutputEvent::Cancelled {
                            seq, order_id, remaining: Qty(remaining),
                            reason: CancelReason::Ioc }),
                        TimeInForce::Fok => unreachable!("pre-checked in step 2"),
                    }
                }
            }

            InputEvent::Cancel { seq, order_id, trader } => { /* §7 */ }
            InputEvent::Modify { .. } => { /* cancel + new, loses priority */ }
        }
    }
}

#[inline(always)]
fn crosses(aggressor: Side, agg_price: Price, resting_price: Price) -> bool {
    match aggressor {
        Side::Bid => agg_price >= resting_price,
        Side::Ask => agg_price <= resting_price,
    }
}
```

### Filling one level

```rust
/// Walk the FIFO oldest-first. Returns the aggressor's remaining qty.
fn fill_at_level(&mut self, /* ... */) -> u64 {
    let mut cur = self.side(opp).levels[level_idx].head;

    while cur != NIL && remaining > 0 {
        // Cache the next pointer BEFORE we potentially free `cur`.
        let next = self.arena.slots[cur as usize].next;

        // --- Self-trade prevention ---
        if stp != SelfTradePrevention::None
            && self.arena.slots[cur as usize].trader == taker_trader {
            match stp {
                SelfTradePrevention::CancelResting => {
                    let rem = self.arena.slots[cur as usize].remaining;
                    let id  = OrderId(self.arena.slots[cur as usize].order_id);
                    self.unlink_and_free(cur, opp, level_idx);
                    out.push(OutputEvent::Cancelled { seq, order_id: id,
                        remaining: Qty(rem), reason: CancelReason::SelfTradePrevention });
                    cur = next;
                    continue;
                }
                SelfTradePrevention::CancelAggressor => {
                    out.push(OutputEvent::Cancelled { seq, order_id: taker_id,
                        remaining: Qty(remaining), reason: CancelReason::SelfTradePrevention });
                    return 0;  // aggressor is dead
                }
                SelfTradePrevention::CancelBoth => { /* both, then continue */ }
                SelfTradePrevention::None => unreachable!(),
            }
        }

        // --- Fill ---
        let maker_rem = self.arena.slots[cur as usize].remaining;
        let fill_qty  = remaining.min(maker_rem);
        let price     = Price(self.arena.slots[cur as usize].price);

        out.push(OutputEvent::Fill {
            seq,
            taker: taker_id,
            maker: OrderId(self.arena.slots[cur as usize].order_id),
            price,                    // ALWAYS the RESTING order's price
            qty: Qty(fill_qty),
            taker_side,
        });

        remaining -= fill_qty;
        self.arena.slots[cur as usize].remaining -= fill_qty;
        self.side_mut(opp).levels[level_idx].total_qty -= fill_qty;

        // --- Maker fully filled → unlink + free ---
        if self.arena.slots[cur as usize].remaining == 0 {
            self.unlink_and_free(cur, opp, level_idx);
        }

        cur = next;
    }

    remaining
}
```

**The price is always the resting order's price.** The aggressor gets price improvement. Getting this backwards is the single most common bug in a first implementation, and the differential fuzzer will catch it in seconds.

**Cache `next` before you free `cur`.** Freeing pushes the slot onto the free-list, which overwrites `next`. Not caching it is a use-after-free-shaped bug that the fuzzer will find immediately.

---

## 7. Unlink: the O(1) cancel

```rust
fn unlink_and_free(&mut self, idx: u32, side: Side, level_idx: u32) {
    let (prev, next) = {
        let s = &self.arena.slots[idx as usize];
        (s.prev, s.next)
    };

    // Splice out of the intrusive doubly-linked list.
    if prev != NIL { self.arena.slots[prev as usize].next = next; }
    else           { self.side_mut(side).levels[level_idx as usize].head = next; }

    if next != NIL { self.arena.slots[next as usize].prev = prev; }
    else           { self.side_mut(side).levels[level_idx as usize].tail = prev; }

    let lvl = &mut self.side_mut(side).levels[level_idx as usize];
    lvl.order_count -= 1;

    // --- Level is now empty: clear the bitmap bit, maybe rescan best ---
    if lvl.order_count == 0 {
        debug_assert_eq!(lvl.head, NIL);
        debug_assert_eq!(lvl.tail, NIL);
        debug_assert_eq!(lvl.total_qty, 0);

        let bs = self.side_mut(side);
        bs.occupied[(level_idx / 64) as usize] &= !(1u64 << (level_idx % 64));

        if bs.best == level_idx {
            match side {
                Side::Bid => bs.rescan_best_bid(level_idx),
                Side::Ask => bs.rescan_best_ask(level_idx),
            }
        }
    }

    self.order_index.remove(OrderId(self.arena.slots[idx as usize].order_id));
    self.arena.free(idx);
}
```

That's the whole cancel path. `order_id → arena idx → unlink → free`. No search. No shifting. O(1).

---

## 8. Invariants

Assert these in `debug_assert!`. Check them in `proptest`. The fuzzer exists to break them.

| # | Invariant |
|---|---|
| I1 | **The book never crosses.** `best_bid_price < best_ask_price` whenever both sides are non-empty. |
| I2 | **Conservation.** For every order: `original_qty == remaining + Σ(fills for that order)`. Nothing is created or destroyed. |
| I3 | **Bitmap consistency.** `occupied[i] set` ⟺ `levels[i].order_count > 0`. |
| I4 | **`best` is correct.** `best` == the true highest (Bid) / lowest (Ask) occupied index. |
| I5 | **Level sum.** `levels[i].total_qty == Σ(remaining)` over the FIFO at level `i`. |
| I6 | **FIFO integrity.** Walking `head → next → ... → NIL` visits exactly `order_count` slots and ends at `tail`. `prev` links mirror it exactly. |
| I7 | **Index bijection.** Every live arena slot appears in `order_index` exactly once, and vice versa. |
| I8 | **Free-list disjointness.** No arena index is simultaneously live and on the free-list. |
| I9 | **No zero-qty resting orders.** A slot with `remaining == 0` must not be linked into any level. |
| I10 | **Determinism.** `hash(state_after(log))` is identical across runs, machines, and builds. |

A `validate()` method should check all ten and is called after every event in `debug` builds and in every `proptest` case. It must be `#[cfg(debug_assertions)]`-gated so it costs nothing in release.

---

## 9. The reference engine

~200 lines. Deliberately slow. **Obviously correct by inspection.** This is your oracle — if you cannot look at it and be certain it is right, it is too clever.

```rust
pub struct ReferenceBook {
    bids: Vec<RefOrder>,  // kept sorted: price DESC, then seq ASC
    asks: Vec<RefOrder>,  // kept sorted: price ASC,  then seq ASC
    next_seq: u64,
}

#[derive(Clone)]
struct RefOrder {
    id: OrderId, trader: TraderId, price: Price,
    remaining: u64, original: u64, seq: u64, stp: SelfTradePrevention,
}

impl ReferenceBook {
    pub fn apply(&mut self, ev: InputEvent) -> Vec<OutputEvent> {
        // New:
        //   linear scan the opposite side (already sorted, so it's in priority order)
        //   fill greedily while crosses()
        //   retain(|o| o.remaining > 0)
        //   if remaining > 0 && GTC: push, then sort_by(price, seq)
        // Cancel:
        //   position(|o| o.id == id), remove
        //
        // Allocate freely. Sort every time. Clone whatever. Who cares.
        // It is correct, and that is its ONLY job.
    }
}
```

**Rules for the reference engine:**
- It must produce **byte-identical `OutputEvent` streams** to the fast engine, in the same order.
- Never optimize it. If you find yourself adding an index to it, stop.
- If the two disagree, the fast engine is guilty until proven innocent — but check the reference too. Sometimes the oracle is wrong, and that's a great story for the README.

---

## 10. Determinism enforcement

Mechanical, not aspirational.

**1. The core crate is `no_std`.** It literally cannot call `Instant::now()` or `rand()`. The type system enforces this.

**2. A global allocator that panics.** Prove zero allocation in the hot path:

```rust
#[cfg(feature = "no-alloc-check")]
struct PanicAllocator;

#[cfg(feature = "no-alloc-check")]
unsafe impl GlobalAlloc for PanicAllocator {
    unsafe fn alloc(&self, _: Layout) -> *mut u8 {
        panic!("allocation in the hot path");
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}
```

Flip it on *after* startup allocation, run the benchmark, and if it doesn't panic you have proof. Put that in the README.

**3. State hashing.** After every event, hash the full book state:

```rust
pub fn state_hash(&self) -> u64 {
    // Hash: every live order (id, price, remaining, trader), in a
    // CANONICAL order (walk levels ascending, FIFO head→tail).
    // Hash: every level's (idx, total_qty, order_count).
    // Do NOT hash arena indices or the free-list — those are
    // implementation detail and MAY legitimately differ between runs.
}
```

Hashing arena indices is the classic mistake here. Two runs can allocate the same *logical* book into different *physical* slots and still be perfectly deterministic at the semantic level. Hash what the book *means*, not where it lives.

**4. The replay test.**

```rust
#[test]
fn replay_is_deterministic() {
    let log = generate_log(seed: 42, n: 1_000_000);
    let baseline = run(&log);
    for _ in 0..100 {
        assert_eq!(run(&log), baseline);  // outputs AND final state hash
    }
}
```

---

## 11. The shell (outside the core)

Everything the engine is forbidden to do.

```
      network / file / stdin
              │
              ▼
      ┌───────────────┐
      │  DECODER      │  wire bytes → InputEvent
      │  + SEQUENCER  │  assign Seq, assign OrderId, stamp wall-clock
      └───────┬───────┘
              │
        SPSC ring buffer (crossbeam / hand-rolled)
              │
              ▼
      ┌───────────────┐
      │  ENGINE CORE  │  single-threaded. pinned to a core. no clock.
      └───────┬───────┘
              │
        SPSC ring buffer
              │
              ▼
      ┌───────────────┐
      │  ENCODER      │  OutputEvent → wire / log / TUI
      │  + JOURNAL    │  append-only input log (for replay)
      └───────────────┘
```

The shell reads the clock. The shell assigns sequence numbers. The shell talks to the network. **The engine does none of these**, and that is the whole point.

Pin the engine thread to an isolated core (`taskset` / `isolcpus`). Set `nice -20`. Disable frequency scaling before you benchmark, or your p99.99 numbers are fiction.

---

## 12. Known-hard cases (write these tests first)

The fuzzer will find these. Save yourself the time and test them up front.

1. Aggressor exactly consumes one full level and stops → is the bitmap bit cleared? Is `best` rescanned?
2. Aggressor sweeps three levels and rests the remainder at a fourth.
3. Cancel the only order at the best level → does `best` rescan correctly?
4. Cancel the only order in the *entire* book → does `best` become `NIL`?
5. FOK that can *almost* fill → must reject and **leave the book completely untouched.**
6. IOC that partially fills → fills, then `Cancelled { reason: Ioc }` for the remainder.
7. Self-trade against your own order at the *middle* of a FIFO → the list must splice correctly.
8. Duplicate `OrderId` while the first is still live → reject.
9. Cancel a non-existent / already-filled order → reject.
10. Arena exhaustion → reject cleanly, do not panic, do not corrupt the book.
11. A fill that takes `remaining` to exactly 0 → unlink, do not leave a zero-qty ghost.
12. `Modify` to the same price → still loses time priority (document this).
13. Price exactly at `min_price` and exactly at `max_price` → boundary of the flat array.
14. A single level with 100,000 orders in the FIFO → does the walk stay linear?
15. Bitmap word boundary: level 63 → 64, and level 64 → 63.

Case 15 is the sneaky one. Off-by-one across a `u64` boundary in `rescan_best` is a bug you will absolutely write, and it will only manifest when the book happens to straddle a multiple of 64.
