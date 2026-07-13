//! Invariant checker: I1–I9 from the technical design.
//!
//! (I2 conservation and I10 determinism are *history* properties — they
//! live in the property-test and replay suites, which see the full event
//! stream. Everything checkable from a state snapshot is checked here.)
//!
//! This is a cold diagnostic path, called after every event in tests and
//! by the fuzzer — never from the hot path. It is compiled always (so a
//! release-mode fuzzer can still validate) but costs nothing unless
//! called.

use crate::arena::NIL;
use crate::book::OrderBook;
use crate::book_side::BookSide;
use crate::types::Side;
use alloc::vec;

impl OrderBook {
    /// Check every state invariant. Returns the first violation.
    pub fn validate(&self) -> Result<(), &'static str> {
        let live_flags = self.validate_side(Side::Bid)?;
        let mut live = live_flags;
        for (i, f) in self.validate_side(Side::Ask)?.iter().enumerate() {
            if *f {
                if live[i] {
                    return Err("I8: slot linked into both sides");
                }
                live[i] = true;
            }
        }

        // I1: the book never crosses.
        let (bb, ba) = (self.bids.best(), self.asks.best());
        if bb != NIL && ba != NIL && self.cfg.idx_to_price(bb) >= self.cfg.idx_to_price(ba) {
            return Err("I1: book is crossed");
        }

        // I7 (count side): every index entry points at a live slot; counts
        // agree. (Per-order direction was checked during the side walks.)
        let linked = live.iter().filter(|&&x| x).count() as u32;
        if linked != self.arena.len() {
            return Err("I7/I8: linked slot count != arena live count");
        }
        if self.index.len() != self.arena.len() {
            return Err("I7: index size != arena live count");
        }

        // I8: free-list disjointness and completeness.
        let free = self.arena.free_list_indices();
        if free.len() as u32 + self.arena.len() != self.arena.capacity() {
            return Err("I8: free list length + live count != capacity");
        }
        let mut seen_free = vec![false; self.arena.capacity() as usize];
        for &f in &free {
            if live[f as usize] {
                return Err("I8: slot is both live and on the free-list");
            }
            if seen_free[f as usize] {
                return Err("I8: free-list contains a cycle/duplicate");
            }
            seen_free[f as usize] = true;
        }

        Ok(())
    }

    /// Walk one side; returns which arena slots are linked into it.
    fn validate_side(&self, side: Side) -> Result<alloc::vec::Vec<bool>, &'static str> {
        let bs: &BookSide = self.side(side);
        let mut live = vec![false; self.arena.capacity() as usize];
        let mut best_seen = NIL;

        for lvl_idx in 0..bs.num_levels() {
            let lvl = bs.level(lvl_idx);

            // I3: bitmap consistency.
            if bs.bit(lvl_idx) != (lvl.order_count > 0) {
                return Err("I3: bitmap bit disagrees with order_count");
            }
            if lvl.order_count == 0 {
                if lvl.head != NIL || lvl.tail != NIL {
                    return Err("I6: empty level with linked head/tail");
                }
                if lvl.total_qty != 0 {
                    return Err("I5: empty level with non-zero total_qty");
                }
                continue;
            }

            if best_seen == NIL
                || match side {
                    Side::Bid => lvl_idx > best_seen,
                    Side::Ask => lvl_idx < best_seen,
                }
            {
                best_seen = lvl_idx;
            }

            // I5 + I6 + I9: walk the FIFO.
            let mut sum: u128 = 0;
            let mut count: u32 = 0;
            let mut prev = NIL;
            let mut cur = lvl.head;
            while cur != NIL {
                if count > lvl.order_count {
                    return Err("I6: FIFO walk exceeds order_count (cycle?)");
                }
                let s = self.arena.slot(cur);
                if s.prev != prev {
                    return Err("I6: prev link does not mirror next link");
                }
                if s.level_idx != lvl_idx {
                    return Err("I6: slot's level_idx disagrees with its level");
                }
                if Side::from_u8(s.side) != side {
                    return Err("I6: slot's side disagrees with its book side");
                }
                if s.remaining == 0 {
                    return Err("I9: zero-qty order linked into a level");
                }
                // I7 (per order): the index maps this order back to this slot.
                if self.index.get(s.order_id) != Some(cur) {
                    return Err("I7: live order missing/mismapped in the index");
                }
                if live[cur as usize] {
                    return Err("I6: slot linked twice");
                }
                live[cur as usize] = true;
                sum += s.remaining as u128;
                count += 1;
                prev = cur;
                cur = s.next;
            }
            if count != lvl.order_count {
                return Err("I6: FIFO length != order_count");
            }
            if prev != lvl.tail {
                return Err("I6: FIFO walk does not end at tail");
            }
            if sum != lvl.total_qty {
                return Err("I5: level total_qty != sum of remaining");
            }
        }

        // I4: the cached best is the true best.
        if bs.best() != best_seen {
            return Err("I4: cached best != true best");
        }
        if bs.best() != bs.scan_true_best() {
            return Err("I4: cached best != bitmap scan best");
        }

        Ok(live)
    }
}
