//! The oracle: a deliberately slow, obviously correct matching engine.
//!
//! Sorted `Vec` per side, linear scans, allocation everywhere, re-sort on
//! every insert. Its only job is to be correct *by inspection* — every
//! function should be readable top to bottom with no cleverness to audit.
//!
//! RULES (from CLAUDE.md):
//! - This engine is written BEFORE the fast engine. It is the spec.
//! - Never optimize it. If you are adding an index to it, stop.
//! - It must produce byte-identical `OutputEvent` streams to the fast
//!   engine, in the same order.

use tessera_core::{
    BookConfig, CancelReason, InputEvent, OrderId, OutputEvent, Price, Qty, RejectReason,
    SelfTradePrevention, Seq, Side, TimeInForce, TraderId,
};

#[derive(Clone, Debug)]
pub struct RefOrder {
    pub id: OrderId,
    pub trader: TraderId,
    pub side: Side,
    pub price: Price,
    pub remaining: u64,
    pub original: u64,
    /// Seq of the input event that placed this order on the book (the
    /// original New, or the Modify that re-priced it). Used only as the
    /// time-priority tiebreak within a price level.
    pub seq: u64,
    pub stp: SelfTradePrevention,
}

pub struct ReferenceBook {
    cfg: BookConfig,
    /// Kept sorted: price DESC, then seq ASC. Front = highest priority.
    bids: Vec<RefOrder>,
    /// Kept sorted: price ASC, then seq ASC. Front = highest priority.
    asks: Vec<RefOrder>,
}

/// Does an aggressor at `agg_price` cross a resting order at `resting_price`?
fn crosses(aggressor: Side, agg_price: Price, resting_price: Price) -> bool {
    match aggressor {
        Side::Bid => agg_price >= resting_price,
        Side::Ask => agg_price <= resting_price,
    }
}

impl ReferenceBook {
    pub fn new(cfg: BookConfig) -> Self {
        ReferenceBook {
            cfg,
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }

    pub fn config(&self) -> &BookConfig {
        &self.cfg
    }

    pub fn apply(&mut self, ev: InputEvent) -> Vec<OutputEvent> {
        let mut out = Vec::new();
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
            } => {
                // -- Validation, in the fixed order both engines share. --
                if qty.0 == 0 {
                    out.push(rejected(seq, order_id, RejectReason::ZeroQty));
                    return out;
                }
                if self.find(order_id).is_some() {
                    out.push(rejected(seq, order_id, RejectReason::DuplicateOrderId));
                    return out;
                }
                if self.cfg.price_to_idx(price).is_none() {
                    out.push(rejected(seq, order_id, RejectReason::PriceOutOfBounds));
                    return out;
                }
                if self.live_count() >= self.cfg.max_live_orders as usize {
                    out.push(rejected(seq, order_id, RejectReason::ArenaFull));
                    return out;
                }
                if tif == TimeInForce::Fok && !self.can_fill_fully(side, price, qty, trader, stp) {
                    out.push(rejected(seq, order_id, RejectReason::FokUnfillable));
                    return out;
                }
                out.push(OutputEvent::Ack { seq, order_id });
                self.do_new(seq, order_id, trader, side, price, qty, tif, stp, &mut out);
            }

            InputEvent::Cancel {
                seq,
                order_id,
                trader,
            } => {
                let Some(o) = self.find(order_id) else {
                    out.push(rejected(seq, order_id, RejectReason::UnknownOrderId));
                    return out;
                };
                if o.trader != trader {
                    out.push(rejected(seq, order_id, RejectReason::WrongTrader));
                    return out;
                }
                let remaining = Qty(o.remaining);
                self.remove(order_id);
                out.push(OutputEvent::Cancelled {
                    seq,
                    order_id,
                    remaining,
                    reason: CancelReason::User,
                });
            }

            InputEvent::Modify {
                seq,
                order_id,
                trader,
                new_price,
                new_qty,
            } => {
                // Validate everything BEFORE touching the book: a rejected
                // modify must leave the book untouched.
                let Some(o) = self.find(order_id) else {
                    out.push(rejected(seq, order_id, RejectReason::UnknownOrderId));
                    return out;
                };
                if o.trader != trader {
                    out.push(rejected(seq, order_id, RejectReason::WrongTrader));
                    return out;
                }
                if new_qty.0 == 0 {
                    out.push(rejected(seq, order_id, RejectReason::ZeroQty));
                    return out;
                }
                if self.cfg.price_to_idx(new_price).is_none() {
                    out.push(rejected(seq, order_id, RejectReason::PriceOutOfBounds));
                    return out;
                }

                // Modify = cancel + new. The new leg re-enters the match
                // loop with a fresh seq, so it ALWAYS loses time priority —
                // even when the price is unchanged. Deliberate.
                let (side, stp, remaining) = (o.side, o.stp, Qty(o.remaining));
                self.remove(order_id);
                out.push(OutputEvent::Cancelled {
                    seq,
                    order_id,
                    remaining,
                    reason: CancelReason::User,
                });
                out.push(OutputEvent::Ack { seq, order_id });
                self.do_new(
                    seq,
                    order_id,
                    trader,
                    side,
                    new_price,
                    new_qty,
                    TimeInForce::Gtc,
                    stp,
                    &mut out,
                );
            }
        }
        out
    }

    /// The match loop: cross against the opposite side in priority order,
    /// then rest (GTC) or kill (IOC) the remainder. Assumes validation and
    /// the Ack already happened.
    #[allow(clippy::too_many_arguments)]
    fn do_new(
        &mut self,
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        side: Side,
        price: Price,
        qty: Qty,
        tif: TimeInForce,
        stp: SelfTradePrevention,
        out: &mut Vec<OutputEvent>,
    ) {
        let mut remaining = qty.0;
        let mut killed_by_stp = false;

        // The opposite side is sorted best-first, so scanning from the
        // front visits resting orders in exact price-time priority.
        let opp = match side {
            Side::Bid => &mut self.asks,
            Side::Ask => &mut self.bids,
        };

        // Every iteration either consumes the FRONT order (index 0) or
        // exits the loop, so the scan position never advances past 0.
        while remaining > 0 && !opp.is_empty() {
            if !crosses(side, price, opp[0].price) {
                break; // no longer marketable
            }

            // -- Self-trade prevention --
            if stp != SelfTradePrevention::None && opp[0].trader == trader {
                match stp {
                    SelfTradePrevention::CancelResting => {
                        let resting = opp.remove(0);
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: resting.id,
                            remaining: Qty(resting.remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        continue; // the next order slides to the front
                    }
                    SelfTradePrevention::CancelAggressor => {
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id,
                            remaining: Qty(remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        killed_by_stp = true;
                        break;
                    }
                    SelfTradePrevention::CancelBoth => {
                        let resting = opp.remove(0);
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id: resting.id,
                            remaining: Qty(resting.remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        out.push(OutputEvent::Cancelled {
                            seq,
                            order_id,
                            remaining: Qty(remaining),
                            reason: CancelReason::SelfTradePrevention,
                        });
                        killed_by_stp = true;
                        break;
                    }
                    SelfTradePrevention::None => unreachable!(),
                }
            }

            // -- Fill. ALWAYS at the resting order's price. --
            let fill_qty = remaining.min(opp[0].remaining);
            out.push(OutputEvent::Fill {
                seq,
                taker: order_id,
                maker: opp[0].id,
                price: opp[0].price,
                qty: Qty(fill_qty),
                taker_side: side,
            });
            remaining -= fill_qty;
            opp[0].remaining -= fill_qty;
            if opp[0].remaining == 0 {
                opp.remove(0); // fully filled maker leaves the book
            }
            // If the maker was only partially filled, `remaining` is now 0
            // and the loop exits on its own.
        }

        if killed_by_stp || remaining == 0 {
            return;
        }

        match tif {
            TimeInForce::Gtc => {
                let side_vec = match side {
                    Side::Bid => &mut self.bids,
                    Side::Ask => &mut self.asks,
                };
                side_vec.push(RefOrder {
                    id: order_id,
                    trader,
                    side,
                    price,
                    remaining,
                    original: qty.0,
                    seq: seq.0,
                    stp,
                });
                // Re-sort the whole side. Slow. Obvious. Correct.
                match side {
                    Side::Bid => {
                        side_vec.sort_by(|a, b| b.price.cmp(&a.price).then(a.seq.cmp(&b.seq)))
                    }
                    Side::Ask => {
                        side_vec.sort_by(|a, b| a.price.cmp(&b.price).then(a.seq.cmp(&b.seq)))
                    }
                }
            }
            TimeInForce::Ioc => {
                out.push(OutputEvent::Cancelled {
                    seq,
                    order_id,
                    remaining: Qty(remaining),
                    reason: CancelReason::Ioc,
                });
            }
            TimeInForce::Fok => {
                // The pre-check guarantees a FOK either fills fully or was
                // rejected. Reaching here means the pre-check is wrong;
                // scream in debug, degrade to an IOC-style kill in release
                // so no input can crash the engine.
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

    /// FOK pre-check: can `qty` fill completely without mutating anything?
    ///
    /// STP-aware, because STP changes what is fillable:
    /// - `CancelResting`: own resting orders would be cancelled, not
    ///   filled, so they contribute nothing and are skipped.
    /// - `CancelAggressor` / `CancelBoth`: hitting an own order kills the
    ///   aggressor, so only quantity resting AHEAD of the first own order
    ///   counts.
    fn can_fill_fully(
        &self,
        side: Side,
        price: Price,
        qty: Qty,
        trader: TraderId,
        stp: SelfTradePrevention,
    ) -> bool {
        let opp = match side {
            Side::Bid => &self.asks,
            Side::Ask => &self.bids,
        };
        let mut available: u128 = 0;
        for o in opp {
            if !crosses(side, price, o.price) {
                break;
            }
            if stp != SelfTradePrevention::None && o.trader == trader {
                match stp {
                    SelfTradePrevention::CancelResting => continue,
                    SelfTradePrevention::CancelAggressor | SelfTradePrevention::CancelBoth => break,
                    SelfTradePrevention::None => unreachable!(),
                }
            }
            available += o.remaining as u128;
            if available >= qty.0 as u128 {
                return true;
            }
        }
        false
    }

    // ---- Introspection (for tests and the differential harness only) ----

    pub fn find(&self, id: OrderId) -> Option<&RefOrder> {
        self.bids
            .iter()
            .chain(self.asks.iter())
            .find(|o| o.id == id)
    }

    fn remove(&mut self, id: OrderId) {
        self.bids.retain(|o| o.id != id);
        self.asks.retain(|o| o.id != id);
    }

    pub fn live_count(&self) -> usize {
        self.bids.len() + self.asks.len()
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.first().map(|o| o.price)
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.first().map(|o| o.price)
    }

    /// All live orders on one side, in exact price-time priority order.
    pub fn side_orders(&self, side: Side) -> &[RefOrder] {
        match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        }
    }

    /// Semantic state hash, byte-identical to `OrderBook::state_hash`:
    /// bids then asks, levels ascending by index, orders FIFO (seq order)
    /// within a level. Written in the oracle's slow-and-obvious style —
    /// a full level scan per side — so the two implementations share
    /// nothing but the canonical definition.
    pub fn state_hash(&self) -> u64 {
        let mut h = tessera_core::hash::Fnv1a::new();
        for side in [Side::Bid, Side::Ask] {
            h.write_u8(side.to_u8());
            let orders = self.side_orders(side);
            for lvl_idx in 0..self.cfg.num_levels {
                // The side vec is price-then-seq sorted, so filtering one
                // level preserves FIFO order.
                let at_level: Vec<&RefOrder> = orders
                    .iter()
                    .filter(|o| self.cfg.price_to_idx(o.price) == Some(lvl_idx))
                    .collect();
                if at_level.is_empty() {
                    continue;
                }
                let total: u128 = at_level.iter().map(|o| o.remaining as u128).sum();
                h.write_u32(lvl_idx);
                h.write_u128(total);
                h.write_u32(at_level.len() as u32);
                for o in at_level {
                    h.write_u64(o.id.0);
                    h.write_i64(o.price.0);
                    h.write_u64(o.remaining);
                    h.write_u32(o.trader.0);
                }
            }
        }
        h.finish()
    }
}

fn rejected(seq: Seq, order_id: OrderId, reason: RejectReason) -> OutputEvent {
    OutputEvent::Rejected {
        seq,
        order_id,
        reason,
    }
}
