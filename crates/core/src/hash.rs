//! Canonical state hashing (invariant I10).
//!
//! `state_hash()` hashes what the book MEANS, never where it lives:
//! live orders in canonical order (levels ascending, FIFO head→tail)
//! plus per-level aggregates. Arena indices and the free-list are
//! implementation detail — two deterministic runs may place the same
//! logical book in different physical slots — so they are NEVER hashed.
//!
//! The hasher is FNV-1a over explicit little-endian bytes: no SipHash
//! keys, no per-process seeds, no platform dependence. The reference
//! engine reuses this exact hasher and canonical order, which makes
//! `state_hash` itself differentially testable.

use crate::arena::NIL;
use crate::book::OrderBook;
use crate::types::Side;

/// FNV-1a, 64-bit. Deterministic across runs, machines, and builds.
pub struct Fnv1a(u64);

impl Fnv1a {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Fnv1a(Self::OFFSET)
    }

    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    #[inline]
    pub fn write_u8(&mut self, v: u8) {
        self.write_bytes(&[v]);
    }

    #[inline]
    pub fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub fn write_u128(&mut self, v: u128) {
        self.write_bytes(&v.to_le_bytes());
    }

    #[inline]
    pub fn write_i64(&mut self, v: i64) {
        self.write_bytes(&v.to_le_bytes());
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

impl Default for Fnv1a {
    fn default() -> Self {
        Self::new()
    }
}

/// Allow `#[derive(Hash)]` types (e.g. `OutputEvent`) to feed an FNV
/// stream, so test harnesses can fingerprint whole output logs.
impl core::hash::Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }
}

impl OrderBook {
    /// Hash the full semantic book state. Canonical order: bids then
    /// asks; levels ascending by index; orders FIFO head→tail.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        for side in [Side::Bid, Side::Ask] {
            h.write_u8(side.to_u8());
            let bs = self.side(side);
            for lvl_idx in 0..bs.num_levels() {
                if !bs.bit(lvl_idx) {
                    continue;
                }
                let lvl = bs.level(lvl_idx);
                h.write_u32(lvl_idx);
                h.write_u128(lvl.total_qty);
                h.write_u32(lvl.order_count);
                let mut cur = lvl.head;
                while cur != NIL {
                    let s = self.arena.slot(cur);
                    h.write_u64(s.order_id);
                    h.write_i64(s.price);
                    h.write_u64(s.remaining);
                    h.write_u32(s.trader);
                    cur = s.next;
                }
            }
        }
        h.finish()
    }
}
