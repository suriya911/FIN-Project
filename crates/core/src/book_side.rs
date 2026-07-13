//! One side of the book: a flat array of price levels plus an occupancy
//! bitmap and a cached best-level index.
//!
//! - Level lookup is `(price - min_price) / tick_size` — O(1), no hashing,
//!   no tree, no pointer chasing.
//! - Best-price lookup is the cached `best` index; it is only rescanned
//!   when the best level EMPTIES, and the rescan is a `leading_zeros` /
//!   `trailing_zeros` over the bitmap — usually a single word.
//! - Each level is an intrusive doubly-linked FIFO through the arena, so
//!   price-time priority falls out for free: the array gives price order,
//!   the FIFO gives time order. Nothing is ever sorted.

use crate::arena::{Arena, NIL};
use crate::types::Side;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Copy, Clone, Debug)]
pub struct Level {
    /// Arena index of the OLDEST order — fills start here.
    pub head: u32,
    /// Arena index of the NEWEST order — resting appends here.
    pub tail: u32,
    /// Sum of `remaining` over the FIFO. u128 so no sum of u64 quantities
    /// can overflow it. Maintained incrementally; used by the FOK
    /// pre-check and invariant I5.
    pub total_qty: u128,
    pub order_count: u32,
}

const EMPTY_LEVEL: Level = Level {
    head: NIL,
    tail: NIL,
    total_qty: 0,
    order_count: 0,
};

pub struct BookSide {
    /// Flat array of levels. Index = (price - min_price) / tick_size.
    levels: Vec<Level>,
    /// Occupancy bitmap: bit i set <=> levels[i] is non-empty.
    occupied: Vec<u64>,
    /// Cached best level index: highest occupied for bids, lowest for
    /// asks. NIL when the side is empty. THE critical optimization.
    best: u32,
    side: Side,
}

impl BookSide {
    pub fn new(side: Side, num_levels: u32) -> Self {
        let words = (num_levels as usize).div_ceil(64);
        BookSide {
            levels: vec![EMPTY_LEVEL; num_levels as usize],
            occupied: vec![0u64; words],
            best: NIL,
            side,
        }
    }

    #[inline(always)]
    pub fn side(&self) -> Side {
        self.side
    }

    #[inline(always)]
    pub fn best(&self) -> u32 {
        self.best
    }

    #[inline(always)]
    pub fn level(&self, idx: u32) -> &Level {
        &self.levels[idx as usize]
    }

    #[inline(always)]
    pub fn level_mut(&mut self, idx: u32) -> &mut Level {
        &mut self.levels[idx as usize]
    }

    #[inline(always)]
    pub fn num_levels(&self) -> u32 {
        self.levels.len() as u32
    }

    #[inline(always)]
    fn set_bit(&mut self, idx: u32) {
        self.occupied[(idx / 64) as usize] |= 1u64 << (idx % 64);
    }

    #[inline(always)]
    fn clear_bit(&mut self, idx: u32) {
        self.occupied[(idx / 64) as usize] &= !(1u64 << (idx % 64));
    }

    #[inline(always)]
    pub fn bit(&self, idx: u32) -> bool {
        self.occupied[(idx / 64) as usize] & (1u64 << (idx % 64)) != 0
    }

    /// Highest occupied level strictly below `from`, or NIL. This IS
    /// `rescan_best_bid`: called only when the best bid level empties.
    pub fn next_occupied_below(&self, from: u32) -> u32 {
        let mut word = (from / 64) as usize;
        let bit = from % 64;
        // Mask off bits at/above `from` in the starting word, scan down.
        let mut w = self.occupied[word] & ((1u64 << bit) - 1);
        loop {
            if w != 0 {
                return (word as u32) * 64 + (63 - w.leading_zeros());
            }
            if word == 0 {
                return NIL;
            }
            word -= 1;
            w = self.occupied[word];
        }
    }

    /// Lowest occupied level strictly above `from`, or NIL. This IS
    /// `rescan_best_ask`: called only when the best ask level empties.
    pub fn next_occupied_above(&self, from: u32) -> u32 {
        let mut word = (from / 64) as usize;
        let bit = from % 64;
        // Mask off bits at/below `from` in the starting word, scan up.
        // Careful at bit 63: `<< 64` would overflow.
        let mut w = if bit == 63 {
            0
        } else {
            self.occupied[word] & (u64::MAX << (bit + 1))
        };
        loop {
            if w != 0 {
                return (word as u32) * 64 + w.trailing_zeros();
            }
            word += 1;
            if word >= self.occupied.len() {
                return NIL;
            }
            w = self.occupied[word];
        }
    }

    /// Next occupied level in the direction of WORSE prices for this side
    /// (used to walk the book best-first without mutating anything).
    #[inline]
    pub fn next_occupied_worse(&self, from: u32) -> u32 {
        match self.side {
            Side::Bid => self.next_occupied_below(from),
            Side::Ask => self.next_occupied_above(from),
        }
    }

    /// Append order `idx` to the tail of level `level_idx`'s FIFO and
    /// account for its quantity. Updates the bitmap and cached best.
    pub fn link_tail(&mut self, arena: &mut Arena, level_idx: u32, idx: u32) {
        let old_tail = self.levels[level_idx as usize].tail;
        {
            let slot = arena.slot_mut(idx);
            slot.prev = old_tail;
            slot.next = NIL;
            slot.level_idx = level_idx;
        }
        let remaining = arena.slot(idx).remaining;

        if old_tail != NIL {
            arena.slot_mut(old_tail).next = idx;
        }
        let lvl = &mut self.levels[level_idx as usize];
        lvl.tail = idx;
        if lvl.head == NIL {
            lvl.head = idx;
        }
        lvl.order_count += 1;
        lvl.total_qty += remaining as u128;

        if lvl.order_count == 1 {
            // Level went empty -> occupied: it may be the new best.
            self.set_bit(level_idx);
            let better = match self.side {
                Side::Bid => self.best == NIL || level_idx > self.best,
                Side::Ask => self.best == NIL || level_idx < self.best,
            };
            if better {
                self.best = level_idx;
            }
        }
    }

    /// Splice order `idx` out of its level's FIFO and de-account its
    /// remaining quantity. If the level empties: clear the bitmap bit and,
    /// if it was the best level, rescan the cached best (CLAUDE.md rule 15).
    ///
    /// Does NOT free the arena slot or touch the order index — the book
    /// owns that sequencing.
    pub fn unlink(&mut self, arena: &mut Arena, idx: u32) {
        let (prev, next, level_idx, remaining) = {
            let s = arena.slot(idx);
            (s.prev, s.next, s.level_idx, s.remaining)
        };

        if prev != NIL {
            arena.slot_mut(prev).next = next;
        } else {
            self.levels[level_idx as usize].head = next;
        }
        if next != NIL {
            arena.slot_mut(next).prev = prev;
        } else {
            self.levels[level_idx as usize].tail = prev;
        }

        let lvl = &mut self.levels[level_idx as usize];
        lvl.order_count -= 1;
        lvl.total_qty -= remaining as u128;

        if lvl.order_count == 0 {
            debug_assert_eq!(lvl.head, NIL);
            debug_assert_eq!(lvl.tail, NIL);
            debug_assert_eq!(lvl.total_qty, 0);
            self.clear_bit(level_idx);
            if self.best == level_idx {
                self.best = match self.side {
                    Side::Bid => self.next_occupied_below(level_idx),
                    Side::Ask => self.next_occupied_above(level_idx),
                };
            }
        }
    }

    /// True best according to a full bitmap scan — validator use only (I4).
    pub fn scan_true_best(&self) -> u32 {
        match self.side {
            Side::Bid => {
                let n = self.num_levels();
                if n == 0 {
                    return NIL;
                }
                if self.bit(n - 1) {
                    return n - 1;
                }
                self.next_occupied_below(n - 1)
            }
            Side::Ask => {
                if self.num_levels() == 0 {
                    return NIL;
                }
                if self.bit(0) {
                    return 0;
                }
                self.next_occupied_above(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a side and rest one 1-lot order at each of the given levels.
    fn side_with(side: Side, levels: &[u32]) -> (BookSide, Arena) {
        let mut bs = BookSide::new(side, 256);
        let mut arena = Arena::with_capacity(64);
        for &lvl in levels {
            let idx = arena.alloc().unwrap();
            let s = arena.slot_mut(idx);
            s.remaining = 1;
            s.order_id = lvl as u64;
            bs.link_tail(&mut arena, lvl, idx);
        }
        (bs, arena)
    }

    #[test]
    fn best_tracks_inserts() {
        let (bids, _) = side_with(Side::Bid, &[10, 50, 30]);
        assert_eq!(bids.best(), 50);
        let (asks, _) = side_with(Side::Ask, &[10, 50, 30]);
        assert_eq!(asks.best(), 10);
    }

    #[test]
    fn rescan_on_empty_level() {
        let (mut bids, mut arena) = side_with(Side::Bid, &[10, 50]);
        // Unlink the only order at the best level (50): best must fall to 10.
        let head = bids.level(50).head;
        bids.unlink(&mut arena, head);
        assert_eq!(bids.best(), 10);
        // Empty the whole side: best must become NIL.
        let head = bids.level(10).head;
        bids.unlink(&mut arena, head);
        assert_eq!(bids.best(), NIL);
        assert!(!bids.bit(10));
    }

    /// The sneaky one: off-by-one across a u64 word boundary (levels
    /// 63 <-> 64) in both scan directions.
    #[test]
    fn word_boundary_63_64() {
        // Bid at 64 falls back to 63 (crosses word 1 -> word 0).
        let (mut bids, mut arena) = side_with(Side::Bid, &[63, 64]);
        assert_eq!(bids.best(), 64);
        let head = bids.level(64).head;
        bids.unlink(&mut arena, head);
        assert_eq!(bids.best(), 63);

        // Ask at 63 advances to 64 (crosses word 0 -> word 1).
        let (mut asks, mut arena) = side_with(Side::Ask, &[63, 64]);
        assert_eq!(asks.best(), 63);
        let head = asks.level(63).head;
        asks.unlink(&mut arena, head);
        assert_eq!(asks.best(), 64);
    }

    #[test]
    fn word_boundary_bit_63_mask() {
        // next_occupied_above from bit 63 must not shift by 64.
        let (asks, _) = side_with(Side::Ask, &[63, 200]);
        assert_eq!(asks.next_occupied_above(63), 200);
        // next_occupied_below from bit 0 of a word.
        let (bids, _) = side_with(Side::Bid, &[64, 3]);
        assert_eq!(bids.next_occupied_below(64), 3);
    }

    #[test]
    fn fifo_link_order_and_middle_unlink() {
        let mut bs = BookSide::new(Side::Ask, 128);
        let mut arena = Arena::with_capacity(8);
        let mut ids = Vec::new();
        for i in 0..3u64 {
            let idx = arena.alloc().unwrap();
            let s = arena.slot_mut(idx);
            s.remaining = 5;
            s.order_id = i;
            bs.link_tail(&mut arena, 7, idx);
            ids.push(idx);
        }
        assert_eq!(bs.level(7).order_count, 3);
        assert_eq!(bs.level(7).total_qty, 15);
        assert_eq!(bs.level(7).head, ids[0]);
        assert_eq!(bs.level(7).tail, ids[2]);

        // Splice out the middle order; head->tail must still walk 0 -> 2.
        bs.unlink(&mut arena, ids[1]);
        assert_eq!(bs.level(7).order_count, 2);
        assert_eq!(bs.level(7).total_qty, 10);
        assert_eq!(arena.slot(ids[0]).next, ids[2]);
        assert_eq!(arena.slot(ids[2]).prev, ids[0]);
        assert_eq!(bs.level(7).head, ids[0]);
        assert_eq!(bs.level(7).tail, ids[2]);
    }

    #[test]
    fn boundary_levels_first_and_last() {
        let (mut asks, mut arena) = side_with(Side::Ask, &[0, 255]);
        assert_eq!(asks.best(), 0);
        let head = asks.level(0).head;
        asks.unlink(&mut arena, head);
        assert_eq!(asks.best(), 255);
        assert_eq!(asks.scan_true_best(), 255);
    }
}
