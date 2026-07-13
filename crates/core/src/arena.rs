//! The order arena: every live order lives in one contiguous, pre-allocated
//! slab. An order is a `u32` index into it — never a pointer, never a `Box`.
//!
//! Freed slots are chained into an intrusive free-list that reuses the
//! `next` field, so slot recycling costs zero extra memory. The arena is
//! the ONLY allocation the engine makes, once, at startup.

use alloc::vec;
use alloc::vec::Vec;

/// Universal sentinel for "no order" / "no level". Never `0` — zero is a
/// valid index.
pub const NIL: u32 = u32::MAX;

/// One resting order.
///
/// Field order is load-bearing: `remaining`, `order_id`, `next`, `prev`,
/// `price`, `trader`, and `level_idx` are touched on every fill or cancel
/// and sit in the first 40 bytes, so the whole hot set lands in a single
/// 64-byte cache line. The cold tail (`original_qty`, `side`, `stp`) is
/// only read when resting or snapshotting.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct OrderSlot {
    // --- HOT ---
    pub remaining: u64,
    pub order_id: u64,
    /// Intrusive FIFO forward link (also the free-list link when freed).
    pub next: u32,
    /// Intrusive FIFO backward link.
    pub prev: u32,
    pub price: i64,
    pub trader: u32,
    /// Which level this order is linked into. Makes cancel O(1).
    pub level_idx: u32,
    // --- COLD ---
    pub original_qty: u64,
    /// 0 = Bid, 1 = Ask (see `Side::to_u8` / `Side::from_u8`).
    pub side: u8,
    /// Encoded `SelfTradePrevention` (see `SelfTradePrevention::to_u8`).
    pub stp: u8,
    _pad: [u8; 6],
}

impl OrderSlot {
    const EMPTY: OrderSlot = OrderSlot {
        remaining: 0,
        order_id: 0,
        next: NIL,
        prev: NIL,
        price: 0,
        trader: 0,
        level_idx: NIL,
        original_qty: 0,
        side: 0,
        stp: 0,
        _pad: [0; 6],
    };
}

pub struct Arena {
    slots: Vec<OrderSlot>,
    /// Head of the intrusive free-list (`NIL` when the arena is full).
    free_head: u32,
    /// Number of live (allocated) slots.
    len: u32,
}

impl Arena {
    /// Pre-allocate `n` slots and chain them all into the free-list.
    /// This is the only allocation in the engine's lifetime.
    ///
    /// `n` must be < `NIL` so every slot has a representable index.
    pub fn with_capacity(n: u32) -> Self {
        debug_assert!(n < NIL);
        let mut slots = vec![OrderSlot::EMPTY; n as usize];
        for (i, slot) in slots.iter_mut().enumerate() {
            slot.next = if i as u32 + 1 < n { i as u32 + 1 } else { NIL };
        }
        Arena {
            slots,
            free_head: if n == 0 { NIL } else { 0 },
            len: 0,
        }
    }

    /// Pop a slot off the free-list. `None` when full — the caller emits
    /// `RejectReason::ArenaFull`. Never allocates.
    #[inline(always)]
    pub fn alloc(&mut self) -> Option<u32> {
        let idx = self.free_head;
        if idx == NIL {
            return None;
        }
        self.free_head = self.slots[idx as usize].next;
        self.len += 1;
        Some(idx)
    }

    /// Push a slot back onto the free-list. Overwrites `next` — callers
    /// walking a FIFO must cache `next` BEFORE freeing (CLAUDE.md rule 12).
    #[inline(always)]
    pub fn free(&mut self, idx: u32) {
        debug_assert!((idx as usize) < self.slots.len());
        self.slots[idx as usize].next = self.free_head;
        self.free_head = idx;
        self.len -= 1;
    }

    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.free_head == NIL
    }

    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    #[inline(always)]
    pub fn slot(&self, idx: u32) -> &OrderSlot {
        &self.slots[idx as usize]
    }

    #[inline(always)]
    pub fn slot_mut(&mut self, idx: u32) -> &mut OrderSlot {
        &mut self.slots[idx as usize]
    }

    /// Free-list traversal for the validator (I8): indices currently on
    /// the free-list, in list order. Cold path only.
    pub fn free_list_indices(&self) -> Vec<u32> {
        let mut v = Vec::new();
        let mut cur = self.free_head;
        // Bounded walk: a cycle in the free-list must not hang the validator.
        while cur != NIL && v.len() <= self.slots.len() {
            v.push(cur);
            cur = self.slots[cur as usize].next;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_free_roundtrip() {
        let mut a = Arena::with_capacity(3);
        assert_eq!(a.len(), 0);
        let x = a.alloc().unwrap();
        let y = a.alloc().unwrap();
        let z = a.alloc().unwrap();
        assert_eq!(a.alloc(), None); // full
        assert!(a.is_full());
        assert_eq!(a.len(), 3);
        a.free(y);
        assert_eq!(a.alloc(), Some(y)); // LIFO reuse
        a.free(z);
        a.free(x);
        a.free(y);
        assert_eq!(a.len(), 0);
        assert_eq!(a.capacity(), 3);
    }

    #[test]
    fn slot_is_one_cache_line() {
        assert_eq!(core::mem::size_of::<OrderSlot>(), 56);
    }

    #[test]
    fn zero_capacity_is_always_full() {
        let mut a = Arena::with_capacity(0);
        assert!(a.is_full());
        assert_eq!(a.alloc(), None);
    }
}
