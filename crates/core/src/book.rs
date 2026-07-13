//! The fast engine: `OrderBook::apply` — the match loop.
//!
//! Semantics are defined by the reference oracle (`tessera-reference`);
//! this file must reproduce its output streams byte-for-byte. Every
//! branch here has a mirror over there, in the same order.

use crate::arena::{Arena, NIL};
use crate::book_side::BookSide;
use crate::buffer::EventBuffer;
use crate::config::BookConfig;
use crate::events::{CancelReason, InputEvent, OutputEvent, RejectReason};
use crate::order_index::OrderIndex;
use crate::types::{OrderId, Price, Qty, SelfTradePrevention, Seq, Side, TimeInForce, TraderId};

pub struct OrderBook {
    pub(crate) cfg: BookConfig,
    pub(crate) arena: Arena,
    pub(crate) bids: BookSide,
    pub(crate) asks: BookSide,
    pub(crate) index: OrderIndex,
}

/// Does an aggressor at `agg_price` cross a resting order at `resting_price`?
#[inline(always)]
fn crosses(aggressor: Side, agg_price: Price, resting_price: Price) -> bool {
    match aggressor {
        Side::Bid => agg_price >= resting_price,
        Side::Ask => agg_price <= resting_price,
    }
}

impl OrderBook {
    /// Pre-allocates everything (arena, level arrays, bitmap, index).
    /// After this, the engine never allocates again.
    pub fn new(cfg: BookConfig) -> Self {
        OrderBook {
            cfg,
            arena: Arena::with_capacity(cfg.max_live_orders),
            bids: BookSide::new(Side::Bid, cfg.num_levels),
            asks: BookSide::new(Side::Ask, cfg.num_levels),
            index: OrderIndex::with_capacity(cfg.max_live_orders),
        }
    }

    pub fn config(&self) -> &BookConfig {
        &self.cfg
    }

    #[inline(always)]
    pub(crate) fn side(&self, s: Side) -> &BookSide {
        match s {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    /// Apply one input event; output events are appended to `out`.
    /// This is the entire hot path. It never allocates, never panics on
    /// input, and never reads anything but (state, event).
    pub fn apply(&mut self, ev: InputEvent, out: &mut EventBuffer) {
        match ev {
            InputEvent::New {
                seq,
                order_id,
                trader,
                side,
                price,
                qty,
                tif,
                stp,
            } => self.apply_new(seq, order_id, trader, side, price, qty, tif, stp, out),
            InputEvent::Cancel {
                seq,
                order_id,
                trader,
            } => self.apply_cancel(seq, order_id, trader, out),
            InputEvent::Modify {
                seq,
                order_id,
                trader,
                new_price,
                new_qty,
            } => self.apply_modify(seq, order_id, trader, new_price, new_qty, out),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_new(
        &mut self,
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        side: Side,
        price: Price,
        qty: Qty,
        tif: TimeInForce,
        stp: SelfTradePrevention,
        out: &mut EventBuffer,
    ) {
        // -- Validation, in the fixed order both engines share. --
        if qty.0 == 0 {
            out.push(reject(seq, order_id, RejectReason::ZeroQty));
            return;
        }
        if self.index.contains(order_id.0) {
            out.push(reject(seq, order_id, RejectReason::DuplicateOrderId));
            return;
        }
        let Some(level_idx) = self.cfg.price_to_idx(price) else {
            out.push(reject(seq, order_id, RejectReason::PriceOutOfBounds));
            return;
        };
        if self.arena.is_full() {
            out.push(reject(seq, order_id, RejectReason::ArenaFull));
            return;
        }
        if tif == TimeInForce::Fok && !self.can_fill_fully(side, price, qty, trader, stp) {
            out.push(reject(seq, order_id, RejectReason::FokUnfillable));
            return;
        }

        out.push(OutputEvent::Ack { seq, order_id });

        let (remaining, killed) = self.cross(seq, order_id, trader, side, price, qty.0, stp, out);
        if killed || remaining == 0 {
            return;
        }
        match tif {
            TimeInForce::Gtc => self.rest(
                order_id, trader, side, price, level_idx, remaining, qty.0, stp, out,
            ),
            TimeInForce::Ioc => out.push(OutputEvent::Cancelled {
                seq,
                order_id,
                remaining: Qty(remaining),
                reason: CancelReason::Ioc,
            }),
            TimeInForce::Fok => {
                // Pre-checked in validation; unreachable unless the
                // pre-check is wrong. Scream in debug, degrade to an
                // IOC-style kill in release (mirrors the reference).
                debug_assert!(false, "FOK pre-check violated");
                out.push(OutputEvent::Cancelled {
                    seq,
                    order_id,
                    remaining: Qty(remaining),
                    reason: CancelReason::Ioc,
                });
            }
        }
    }

    fn apply_cancel(
        &mut self,
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        out: &mut EventBuffer,
    ) {
        let Some(idx) = self.index.get(order_id.0) else {
            out.push(reject(seq, order_id, RejectReason::UnknownOrderId));
            return;
        };
        if self.arena.slot(idx).trader != trader.0 {
            out.push(reject(seq, order_id, RejectReason::WrongTrader));
            return;
        }
        let remaining = self.arena.slot(idx).remaining;
        self.unlink_and_free(idx);
        out.push(OutputEvent::Cancelled {
            seq,
            order_id,
            remaining: Qty(remaining),
            reason: CancelReason::User,
        });
    }

    fn apply_modify(
        &mut self,
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        new_price: Price,
        new_qty: Qty,
        out: &mut EventBuffer,
    ) {
        // Validate everything BEFORE touching the book: a rejected modify
        // must leave the book untouched.
        let Some(idx) = self.index.get(order_id.0) else {
            out.push(reject(seq, order_id, RejectReason::UnknownOrderId));
            return;
        };
        if self.arena.slot(idx).trader != trader.0 {
            out.push(reject(seq, order_id, RejectReason::WrongTrader));
            return;
        }
        if new_qty.0 == 0 {
            out.push(reject(seq, order_id, RejectReason::ZeroQty));
            return;
        }
        let Some(level_idx) = self.cfg.price_to_idx(new_price) else {
            out.push(reject(seq, order_id, RejectReason::PriceOutOfBounds));
            return;
        };

        // Modify = cancel + new. The new leg re-enters the match loop, so
        // it ALWAYS loses time priority — even at the same price.
        let (side, stp, old_remaining) = {
            let s = self.arena.slot(idx);
            (
                Side::from_u8(s.side),
                SelfTradePrevention::from_u8(s.stp),
                s.remaining,
            )
        };
        self.unlink_and_free(idx);
        out.push(OutputEvent::Cancelled {
            seq,
            order_id,
            remaining: Qty(old_remaining),
            reason: CancelReason::User,
        });
        out.push(OutputEvent::Ack { seq, order_id });

        let (remaining, killed) =
            self.cross(seq, order_id, trader, side, new_price, new_qty.0, stp, out);
        if !killed && remaining > 0 {
            self.rest(
                order_id, trader, side, new_price, level_idx, remaining, new_qty.0, stp, out,
            );
        }
    }

    /// The cross loop: consume opposite levels best-first while the order
    /// is marketable. Returns (remaining, killed_by_stp).
    #[allow(clippy::too_many_arguments)]
    fn cross(
        &mut self,
        seq: Seq,
        taker_id: OrderId,
        taker_trader: TraderId,
        taker_side: Side,
        price: Price,
        mut remaining: u64,
        stp: SelfTradePrevention,
        out: &mut EventBuffer,
    ) -> (u64, bool) {
        let opp = taker_side.opposite();
        while remaining > 0 {
            let best = self.side(opp).best();
            if best == NIL {
                break; // opposite side is empty
            }
            if !crosses(taker_side, price, self.cfg.idx_to_price(best)) {
                break; // no longer marketable
            }
            let killed;
            (remaining, killed) = self.fill_at_level(
                opp,
                best,
                remaining,
                seq,
                taker_id,
                taker_trader,
                taker_side,
                stp,
                out,
            );
            if killed {
                return (remaining, true);
            }
        }
        (remaining, false)
    }

    /// Walk one level's FIFO oldest-first, filling into `remaining`.
    /// Returns (remaining, killed_by_stp).
    #[allow(clippy::too_many_arguments)]
    fn fill_at_level(
        &mut self,
        opp: Side,
        level_idx: u32,
        mut remaining: u64,
        seq: Seq,
        taker_id: OrderId,
        taker_trader: TraderId,
        taker_side: Side,
        stp: SelfTradePrevention,
        out: &mut EventBuffer,
    ) -> (u64, bool) {
        let mut cur = self.side(opp).level(level_idx).head;

        while cur != NIL && remaining > 0 {
            // Cache `next` BEFORE any free: freeing pushes the slot onto
            // the free-list, which overwrites `next` (CLAUDE.md rule 12).
            let next = self.arena.slot(cur).next;
            let (m_trader, m_rem, m_id, m_price) = {
                let s = self.arena.slot(cur);
                (s.trader, s.remaining, s.order_id, s.price)
            };

            // -- Self-trade prevention --
            if stp != SelfTradePrevention::None && m_trader == taker_trader.0 {
                match stp {
                    SelfTradePrevention::CancelResting => {
                        self.unlink_and_free(cur);
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: OrderId(m_id),
                            remaining: Qty(m_rem),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        cur = next;
                        continue;
                    }
                    SelfTradePrevention::CancelAggressor => {
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: taker_id,
                            remaining: Qty(remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        return (remaining, true);
                    }
                    SelfTradePrevention::CancelBoth => {
                        self.unlink_and_free(cur);
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: OrderId(m_id),
                            remaining: Qty(m_rem),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: taker_id,
                            remaining: Qty(remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        return (remaining, true);
                    }
                    SelfTradePrevention::None => unreachable!(),
                }
            }

            // -- Fill. ALWAYS at the resting (maker) order's price. --
            let fill_qty = remaining.min(m_rem);
            out.push(OutputEvent::Fill {
                seq,
                taker: taker_id,
                maker: OrderId(m_id),
                price: Price(m_price),
                qty: Qty(fill_qty),
                taker_side,
            });
            remaining -= fill_qty;
            self.arena.slot_mut(cur).remaining -= fill_qty;
            match opp {
                Side::Bid => self.bids.level_mut(level_idx).total_qty -= fill_qty as u128,
                Side::Ask => self.asks.level_mut(level_idx).total_qty -= fill_qty as u128,
            }

            // Maker fully filled: unlink and recycle. No zero-qty ghosts.
            if self.arena.slot(cur).remaining == 0 {
                self.unlink_and_free(cur);
            }

            cur = next;
        }

        (remaining, false)
    }

    /// Rest the remainder on the book (GTC only). `level_idx` was computed
    /// and bounds-checked during validation.
    #[allow(clippy::too_many_arguments)]
    fn rest(
        &mut self,
        order_id: OrderId,
        trader: TraderId,
        side: Side,
        price: Price,
        level_idx: u32,
        remaining: u64,
        original_qty: u64,
        stp: SelfTradePrevention,
        out: &mut EventBuffer,
    ) {
        let Some(idx) = self.arena.alloc() else {
            // Unreachable: validation rejects when the arena is full and
            // matching only frees slots. Defensive, never panicking.
            debug_assert!(false, "arena full at rest after validation");
            out.push(reject(Seq(0), order_id, RejectReason::ArenaFull));
            return;
        };
        {
            let s = self.arena.slot_mut(idx);
            s.remaining = remaining;
            s.order_id = order_id.0;
            s.price = price.0;
            s.trader = trader.0;
            s.original_qty = original_qty;
            s.side = side.to_u8();
            s.stp = stp.to_u8();
        }
        match side {
            Side::Bid => self.bids.link_tail(&mut self.arena, level_idx, idx),
            Side::Ask => self.asks.link_tail(&mut self.arena, level_idx, idx),
        }
        self.index.insert(order_id.0, idx);
    }

    /// Splice an order out of its level, drop it from the index, and
    /// recycle its arena slot. O(1) — this IS the cancel path.
    fn unlink_and_free(&mut self, idx: u32) {
        let (side, order_id) = {
            let s = self.arena.slot(idx);
            (Side::from_u8(s.side), s.order_id)
        };
        match side {
            Side::Bid => self.bids.unlink(&mut self.arena, idx),
            Side::Ask => self.asks.unlink(&mut self.arena, idx),
        }
        self.index.remove(order_id);
        self.arena.free(idx);
    }

    /// FOK pre-check: walk levels best-first, sum available quantity,
    /// mutate NOTHING. STP-aware with the same rules as the oracle:
    /// CancelResting skips own orders; CancelAggressor/CancelBoth stop at
    /// the first own order (the aggressor would die there).
    fn can_fill_fully(
        &self,
        side: Side,
        price: Price,
        qty: Qty,
        trader: TraderId,
        stp: SelfTradePrevention,
    ) -> bool {
        let opp = self.side(side.opposite());
        let mut available: u128 = 0;
        let mut lvl = opp.best();
        while lvl != NIL {
            if !crosses(side, price, self.cfg.idx_to_price(lvl)) {
                break;
            }
            if stp == SelfTradePrevention::None {
                // No STP: the level aggregate is exactly what's fillable.
                available += opp.level(lvl).total_qty;
            } else {
                let mut cur = opp.level(lvl).head;
                while cur != NIL {
                    let s = self.arena.slot(cur);
                    if s.trader == trader.0 {
                        match stp {
                            SelfTradePrevention::CancelResting => {
                                cur = s.next;
                                continue;
                            }
                            SelfTradePrevention::CancelAggressor
                            | SelfTradePrevention::CancelBoth => {
                                return available >= qty.0 as u128;
                            }
                            SelfTradePrevention::None => unreachable!(),
                        }
                    }
                    available += s.remaining as u128;
                    if available >= qty.0 as u128 {
                        return true;
                    }
                    cur = s.next;
                }
            }
            if available >= qty.0 as u128 {
                return true;
            }
            lvl = opp.next_occupied_worse(lvl);
        }
        available >= qty.0 as u128
    }

    // ---- Introspection (cold paths: tests, TUI, snapshots) ----

    pub fn best_bid(&self) -> Option<Price> {
        let b = self.bids.best();
        (b != NIL).then(|| self.cfg.idx_to_price(b))
    }

    pub fn best_ask(&self) -> Option<Price> {
        let b = self.asks.best();
        (b != NIL).then(|| self.cfg.idx_to_price(b))
    }

    pub fn live_count(&self) -> u32 {
        self.arena.len()
    }

    /// Top-of-book depth ladder: up to `n` occupied levels best-first as
    /// (price, total quantity, order count). Cold path — TUI and tooling.
    pub fn depth(&self, side: Side, n: usize) -> alloc::vec::Vec<(Price, u128, u32)> {
        let bs = self.side(side);
        let mut out = alloc::vec::Vec::with_capacity(n);
        let mut lvl = bs.best();
        while lvl != NIL && out.len() < n {
            let l = bs.level(lvl);
            out.push((self.cfg.idx_to_price(lvl), l.total_qty, l.order_count));
            lvl = bs.next_occupied_worse(lvl);
        }
        out
    }
}

#[inline(always)]
fn reject(seq: Seq, order_id: OrderId, reason: RejectReason) -> OutputEvent {
    OutputEvent::Rejected {
        seq,
        order_id,
        reason,
    }
}
