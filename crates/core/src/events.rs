//! The engine's entire API surface: one input enum in, one output enum out.
//!
//! Every `OutputEvent` carries the `Seq` of the input that caused it, which
//! makes output logs self-describing and trivially diffable between two
//! engines — the property the differential fuzzer is built on.

use crate::types::{OrderId, Price, Qty, SelfTradePrevention, Seq, Side, TimeInForce, TraderId};

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum InputEvent {
    New {
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        side: Side,
        price: Price,
        qty: Qty,
        tif: TimeInForce,
        stp: SelfTradePrevention,
    },
    Cancel {
        seq: Seq,
        order_id: OrderId,
        /// Must match the resting order's trader or the cancel is rejected.
        trader: TraderId,
    },
    /// Modify = cancel + new. It ALWAYS loses time priority, including a
    /// modify to the same price. This is a deliberate design decision
    /// (it removes the queue-position-preserving special cases that
    /// riddle real venues' modify semantics), not a limitation.
    Modify {
        seq: Seq,
        order_id: OrderId,
        trader: TraderId,
        new_price: Price,
        new_qty: Qty,
    },
}

impl InputEvent {
    #[inline]
    pub fn seq(&self) -> Seq {
        match *self {
            InputEvent::New { seq, .. }
            | InputEvent::Cancel { seq, .. }
            | InputEvent::Modify { seq, .. } => seq,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum OutputEvent {
    Ack {
        seq: Seq,
        order_id: OrderId,
    },
    Fill {
        seq: Seq,
        taker: OrderId,
        maker: OrderId,
        /// ALWAYS the resting (maker) order's price. The aggressor
        /// receives price improvement.
        price: Price,
        qty: Qty,
        taker_side: Side,
    },
    Cancelled {
        seq: Seq,
        order_id: OrderId,
        remaining: Qty,
        reason: CancelReason,
    },
    Rejected {
        seq: Seq,
        order_id: OrderId,
        reason: RejectReason,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum CancelReason {
    /// User-requested cancel (or the cancel leg of a modify).
    User,
    /// Unfilled remainder of an immediate-or-cancel order.
    Ioc,
    SelfTradePrevention,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum RejectReason {
    DuplicateOrderId,
    UnknownOrderId,
    /// Off the price grid: below min, above max, or not a multiple of the
    /// tick size.
    PriceOutOfBounds,
    ZeroQty,
    FokUnfillable,
    /// The book is at capacity. Rejected up front, before matching, so
    /// both engines agree without having to predict whether the order
    /// would have rested.
    ArenaFull,
    WrongTrader,
}
