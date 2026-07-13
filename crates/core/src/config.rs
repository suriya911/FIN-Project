//! Book configuration, shared by the fast engine and the reference oracle
//! so that both enforce identical price-grid and capacity rules.

use crate::types::Price;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BookConfig {
    /// Lowest representable price, in ticks.
    pub min_price: i64,
    /// Price grid step, in ticks. Must be >= 1.
    pub tick_size: i64,
    /// Number of price levels per side. Level i represents price
    /// `min_price + i * tick_size`.
    pub num_levels: u32,
    /// Maximum number of live resting orders (arena capacity).
    pub max_live_orders: u32,
}

impl BookConfig {
    /// A small, human-scale config used across tests.
    pub const TEST: BookConfig = BookConfig {
        min_price: 1_000,
        tick_size: 1,
        num_levels: 1_024,
        max_live_orders: 4_096,
    };

    /// Map a price onto its level index. `None` if the price is below the
    /// grid, above it, or not aligned to the tick size. Never panics,
    /// never wraps.
    #[inline(always)]
    pub fn price_to_idx(&self, p: Price) -> Option<u32> {
        let off = p.0.checked_sub(self.min_price)?;
        if off < 0 || off % self.tick_size != 0 {
            return None;
        }
        let idx = off / self.tick_size;
        if idx >= self.num_levels as i64 {
            return None;
        }
        Some(idx as u32)
    }

    #[inline(always)]
    pub fn idx_to_price(&self, idx: u32) -> Price {
        Price(self.min_price + idx as i64 * self.tick_size)
    }

    #[inline(always)]
    pub fn max_price(&self) -> Price {
        self.idx_to_price(self.num_levels - 1)
    }
}
