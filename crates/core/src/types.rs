//! Core scalar types. Money is never a float: prices are integer ticks,
//! quantities are integer lots.

/// Price in ticks. The real price is `price_ticks * tick_size`, but the
/// engine never materializes that product — everything downstream of the
/// wire speaks ticks.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Price(pub i64);

/// Quantity in lots.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Qty(pub u64);

/// Globally unique order identity, assigned by the shell, not the engine.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct OrderId(pub u64);

/// Participant identity. Used for self-trade prevention and cancel
/// ownership checks.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct TraderId(pub u32);

/// Sequence number of the input event. The engine's ONLY notion of time.
/// A wall-clock timestamp may ride along in the shell's journal for
/// humans, but the matcher never reads one.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Seq(pub u64);

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Side {
    Bid,
    Ask,
}

impl Side {
    #[inline(always)]
    pub fn opposite(self) -> Side {
        match self {
            Side::Bid => Side::Ask,
            Side::Ask => Side::Bid,
        }
    }

    /// Compact encoding for the arena's cold fields and the wire codec.
    #[inline(always)]
    pub fn to_u8(self) -> u8 {
        match self {
            Side::Bid => 0,
            Side::Ask => 1,
        }
    }

    #[inline(always)]
    pub fn from_u8(v: u8) -> Side {
        if v == 0 {
            Side::Bid
        } else {
            Side::Ask
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum TimeInForce {
    /// Good-till-cancel: rest the unfilled remainder on the book.
    Gtc,
    /// Immediate-or-cancel: fill what crosses, cancel the rest.
    Ioc,
    /// Fill-or-kill: fill entirely or reject entirely, leaving the book
    /// untouched.
    Fok,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum SelfTradePrevention {
    None,
    /// Cancel the resting order and keep matching.
    CancelResting,
    /// Cancel the incoming (aggressing) order.
    CancelAggressor,
    /// Cancel both the resting and the incoming order.
    CancelBoth,
}

impl SelfTradePrevention {
    /// Compact encoding for the arena's cold fields and the wire codec.
    #[inline(always)]
    pub fn to_u8(self) -> u8 {
        match self {
            SelfTradePrevention::None => 0,
            SelfTradePrevention::CancelResting => 1,
            SelfTradePrevention::CancelAggressor => 2,
            SelfTradePrevention::CancelBoth => 3,
        }
    }

    #[inline(always)]
    pub fn from_u8(v: u8) -> SelfTradePrevention {
        match v {
            1 => SelfTradePrevention::CancelResting,
            2 => SelfTradePrevention::CancelAggressor,
            3 => SelfTradePrevention::CancelBoth,
            _ => SelfTradePrevention::None,
        }
    }
}
