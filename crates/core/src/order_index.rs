//! OrderId -> arena index, in O(1), with no `std::HashMap`.
//!
//! An open-addressed, linear-probed flat map: power-of-two sized,
//! pre-allocated at startup, never grows, never hashes with SipHash, and
//! never allocates after construction. Sized at 2x the arena capacity
//! (load factor <= 0.5) so probe chains stay short; deletes leave
//! tombstones that inserts reuse.
//!
//! Chosen over the direct-mapped `Vec` variant because the differential
//! fuzzer throws adversarial IDs (colliding, sparse, reused) at the
//! engine, and the flat map handles those with the exact same semantics
//! as the reference oracle.

use alloc::vec;
use alloc::vec::Vec;

const CTRL_EMPTY: u8 = 0;
const CTRL_FULL: u8 = 1;
const CTRL_TOMBSTONE: u8 = 2;

/// splitmix64: a strong, deterministic 64-bit mixer. No per-process seed —
/// determinism rule I10 forbids one.
#[inline(always)]
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

pub struct OrderIndex {
    keys: Vec<u64>,
    vals: Vec<u32>,
    ctrl: Vec<u8>,
    mask: usize,
    len: u32,
}

impl OrderIndex {
    /// Capacity is rounded up to a power of two >= 2 * `max_live` so the
    /// load factor never exceeds 0.5.
    pub fn with_capacity(max_live: u32) -> Self {
        let cap = ((max_live as usize * 2).max(8)).next_power_of_two();
        OrderIndex {
            keys: vec![0; cap],
            vals: vec![0; cap],
            ctrl: vec![CTRL_EMPTY; cap],
            mask: cap - 1,
            len: 0,
        }
    }

    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn get(&self, key: u64) -> Option<u32> {
        let mut i = mix(key) as usize & self.mask;
        // Bounded probe: even a table degraded to all full/tombstone slots
        // terminates after one lap.
        for _ in 0..=self.mask {
            match self.ctrl[i] {
                CTRL_EMPTY => return None,
                CTRL_FULL if self.keys[i] == key => return Some(self.vals[i]),
                _ => i = (i + 1) & self.mask,
            }
        }
        None
    }

    #[inline(always)]
    pub fn contains(&self, key: u64) -> bool {
        self.get(key).is_some()
    }

    /// Insert a key that is NOT already present (the engine rejects
    /// duplicates before calling this). Reuses the first tombstone on the
    /// probe path. Never allocates.
    #[inline]
    pub fn insert(&mut self, key: u64, val: u32) {
        debug_assert!(!self.contains(key), "insert of a live key");
        let mut i = mix(key) as usize & self.mask;
        let mut target = usize::MAX;
        for _ in 0..=self.mask {
            match self.ctrl[i] {
                CTRL_EMPTY => {
                    if target == usize::MAX {
                        target = i;
                    }
                    break;
                }
                CTRL_TOMBSTONE => {
                    if target == usize::MAX {
                        target = i;
                    }
                    i = (i + 1) & self.mask;
                }
                _ => i = (i + 1) & self.mask,
            }
        }
        // len <= max_live <= cap/2 guarantees a free slot exists.
        debug_assert!(target != usize::MAX, "order index full");
        self.ctrl[target] = CTRL_FULL;
        self.keys[target] = key;
        self.vals[target] = val;
        self.len += 1;
    }

    #[inline]
    pub fn remove(&mut self, key: u64) -> Option<u32> {
        let mut i = mix(key) as usize & self.mask;
        for _ in 0..=self.mask {
            match self.ctrl[i] {
                CTRL_EMPTY => return None,
                CTRL_FULL if self.keys[i] == key => {
                    self.ctrl[i] = CTRL_TOMBSTONE;
                    self.len -= 1;
                    return Some(self.vals[i]);
                }
                _ => i = (i + 1) & self.mask,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut m = OrderIndex::with_capacity(16);
        m.insert(42, 7);
        m.insert(0, 1); // 0 is a perfectly valid key
        m.insert(u64::MAX, 2);
        assert_eq!(m.get(42), Some(7));
        assert_eq!(m.get(0), Some(1));
        assert_eq!(m.get(u64::MAX), Some(2));
        assert_eq!(m.get(43), None);
        assert_eq!(m.remove(42), Some(7));
        assert_eq!(m.get(42), None);
        assert_eq!(m.remove(42), None);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn tombstones_do_not_break_probe_chains() {
        let mut m = OrderIndex::with_capacity(4); // table of 8
                                                  // Insert colliding-ish keys, remove one in the middle of the
                                                  // chain, and confirm the ones behind it are still reachable.
        let keys: Vec<u64> = (0..4).collect();
        for (v, &k) in keys.iter().enumerate() {
            m.insert(k, v as u32);
        }
        m.remove(keys[1]);
        for (v, &k) in keys.iter().enumerate() {
            if k == keys[1] {
                continue;
            }
            assert_eq!(m.get(k), Some(v as u32), "key {k} lost after delete");
        }
        // Tombstone gets reused.
        m.insert(keys[1], 99);
        assert_eq!(m.get(keys[1]), Some(99));
    }

    #[test]
    fn churn_at_capacity() {
        // max_live = 4 -> table of 8. Insert/remove far more times than
        // the table size to force tombstone reuse on every probe path.
        let mut m = OrderIndex::with_capacity(4);
        for round in 0..1000u64 {
            for k in 0..4u64 {
                m.insert(round * 4 + k, k as u32);
            }
            for k in 0..4u64 {
                assert_eq!(m.get(round * 4 + k), Some(k as u32));
                assert_eq!(m.remove(round * 4 + k), Some(k as u32));
            }
            assert!(m.is_empty());
        }
    }
}
