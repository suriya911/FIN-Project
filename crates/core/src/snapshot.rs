//! Snapshot / restore: crash recovery and fast-forward replay.
//!
//! A snapshot is the SEMANTIC book state — live orders in canonical
//! order (levels ascending, FIFO head→tail), never arena indices — so
//! `restore(snapshot(book))` rebuilds an equivalent book even though the
//! physical slot assignment may differ. `snapshot_at(N)` + `replay_from(N)`
//! must equal a full replay; the test suite holds that equation.

use crate::arena::NIL;
use crate::book::OrderBook;
use crate::config::BookConfig;
use crate::types::{Price, SelfTradePrevention, Side};
use alloc::vec::Vec;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SnapOrder {
    pub order_id: u64,
    pub trader: u32,
    pub side: Side,
    pub price: i64,
    pub remaining: u64,
    pub original_qty: u64,
    pub stp: SelfTradePrevention,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub cfg: BookConfig,
    /// Live orders in canonical order. Within a level this is FIFO order,
    /// which is exactly what restore needs to rebuild time priority.
    pub orders: Vec<SnapOrder>,
}

impl OrderBook {
    /// Capture the semantic state. Cold path; allocates freely.
    pub fn snapshot(&self) -> Snapshot {
        let mut orders = Vec::with_capacity(self.arena.len() as usize);
        for side in [Side::Bid, Side::Ask] {
            let bs = self.side(side);
            for lvl_idx in 0..bs.num_levels() {
                if !bs.bit(lvl_idx) {
                    continue;
                }
                let mut cur = bs.level(lvl_idx).head;
                while cur != NIL {
                    let s = self.arena.slot(cur);
                    orders.push(SnapOrder {
                        order_id: s.order_id,
                        trader: s.trader,
                        side,
                        price: s.price,
                        remaining: s.remaining,
                        original_qty: s.original_qty,
                        stp: SelfTradePrevention::from_u8(s.stp),
                    });
                    cur = s.next;
                }
            }
        }
        Snapshot {
            cfg: self.cfg,
            orders,
        }
    }

    /// Rebuild a book from a snapshot. Orders are linked directly (no
    /// matching): snapshot order preserves per-level FIFO, so time
    /// priority survives the round trip.
    pub fn restore(snap: &Snapshot) -> Result<OrderBook, &'static str> {
        let mut book = OrderBook::new(snap.cfg);
        for o in &snap.orders {
            let Some(level_idx) = snap.cfg.price_to_idx(Price(o.price)) else {
                return Err("snapshot order price off the grid");
            };
            if o.remaining == 0 {
                return Err("snapshot contains a zero-qty order");
            }
            if book.index.contains(o.order_id) {
                return Err("snapshot contains a duplicate order id");
            }
            let Some(idx) = book.arena.alloc() else {
                return Err("snapshot exceeds book capacity");
            };
            {
                let s = book.arena.slot_mut(idx);
                s.remaining = o.remaining;
                s.order_id = o.order_id;
                s.price = o.price;
                s.trader = o.trader;
                s.original_qty = o.original_qty;
                s.side = o.side.to_u8();
                s.stp = o.stp.to_u8();
            }
            match o.side {
                Side::Bid => book.bids.link_tail(&mut book.arena, level_idx, idx),
                Side::Ask => book.asks.link_tail(&mut book.arena, level_idx, idx),
            }
            book.index.insert(o.order_id, idx);
        }
        Ok(book)
    }
}
