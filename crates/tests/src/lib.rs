//! Cross-engine test harness.
//!
//! Every behavioral test is written once, against the [`Engine`] trait,
//! and instantiated for BOTH the reference oracle and the fast engine.
//! One suite, two engines — the suites cannot drift apart.

use tessera_core::{
    BookConfig, CancelReason, InputEvent, OrderId, OutputEvent, Price, Qty, RejectReason,
    SelfTradePrevention, Seq, Side, TimeInForce, TraderId,
};

/// The narrow waist both engines share.
pub trait Engine {
    fn new(cfg: BookConfig) -> Self;
    fn apply(&mut self, ev: InputEvent) -> Vec<OutputEvent>;
}

impl Engine for tessera_reference::ReferenceBook {
    fn new(cfg: BookConfig) -> Self {
        tessera_reference::ReferenceBook::new(cfg)
    }
    fn apply(&mut self, ev: InputEvent) -> Vec<OutputEvent> {
        tessera_reference::ReferenceBook::apply(self, ev)
    }
}

// ---------------------------------------------------------------------
// Terse constructors: tests read as event-in / events-out tables.
// ---------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn new_full(
    seq: u64,
    id: u64,
    trader: u32,
    side: Side,
    price: i64,
    qty: u64,
    tif: TimeInForce,
    stp: SelfTradePrevention,
) -> InputEvent {
    InputEvent::New {
        seq: Seq(seq),
        order_id: OrderId(id),
        trader: TraderId(trader),
        side,
        price: Price(price),
        qty: Qty(qty),
        tif,
        stp,
    }
}

/// Plain GTC limit order, no self-trade prevention.
pub fn new_lim(seq: u64, id: u64, trader: u32, side: Side, price: i64, qty: u64) -> InputEvent {
    new_full(
        seq,
        id,
        trader,
        side,
        price,
        qty,
        TimeInForce::Gtc,
        SelfTradePrevention::None,
    )
}

pub fn new_tif(
    seq: u64,
    id: u64,
    trader: u32,
    side: Side,
    price: i64,
    qty: u64,
    tif: TimeInForce,
) -> InputEvent {
    new_full(
        seq,
        id,
        trader,
        side,
        price,
        qty,
        tif,
        SelfTradePrevention::None,
    )
}

pub fn cancel(seq: u64, id: u64, trader: u32) -> InputEvent {
    InputEvent::Cancel {
        seq: Seq(seq),
        order_id: OrderId(id),
        trader: TraderId(trader),
    }
}

pub fn modify(seq: u64, id: u64, trader: u32, price: i64, qty: u64) -> InputEvent {
    InputEvent::Modify {
        seq: Seq(seq),
        order_id: OrderId(id),
        trader: TraderId(trader),
        new_price: Price(price),
        new_qty: Qty(qty),
    }
}

pub fn ack(seq: u64, id: u64) -> OutputEvent {
    OutputEvent::Ack {
        seq: Seq(seq),
        order_id: OrderId(id),
    }
}

pub fn fill(
    seq: u64,
    taker: u64,
    maker: u64,
    price: i64,
    qty: u64,
    taker_side: Side,
) -> OutputEvent {
    OutputEvent::Fill {
        seq: Seq(seq),
        taker: OrderId(taker),
        maker: OrderId(maker),
        price: Price(price),
        qty: Qty(qty),
        taker_side,
    }
}

pub fn cancelled(seq: u64, id: u64, remaining: u64, reason: CancelReason) -> OutputEvent {
    OutputEvent::Cancelled {
        seq: Seq(seq),
        order_id: OrderId(id),
        remaining: Qty(remaining),
        reason,
    }
}

pub fn rejected(seq: u64, id: u64, reason: RejectReason) -> OutputEvent {
    OutputEvent::Rejected {
        seq: Seq(seq),
        order_id: OrderId(id),
        reason,
    }
}

// ---------------------------------------------------------------------
// The shared behavioral suite. Each case is generic over the engine.
// ---------------------------------------------------------------------

pub mod cases {
    use super::*;
    use CancelReason as CR;
    use RejectReason as RR;
    use SelfTradePrevention as Stp;
    use Side::{Ask, Bid};
    use TimeInForce as Tif;

    fn eng<E: Engine>() -> E {
        E::new(BookConfig::TEST)
    }

    pub fn rest_and_ack<E: Engine>() {
        let mut e = eng::<E>();
        assert_eq!(e.apply(new_lim(1, 1, 1, Bid, 1500, 10)), vec![ack(1, 1)]);
    }

    pub fn simple_cross_full<E: Engine>() {
        let mut e = eng::<E>();
        assert_eq!(e.apply(new_lim(1, 1, 1, Ask, 1500, 10)), vec![ack(1, 1)]);
        assert_eq!(
            e.apply(new_lim(2, 2, 2, Bid, 1500, 10)),
            vec![ack(2, 2), fill(2, 2, 1, 1500, 10, Bid)]
        );
        // Both orders are gone: a new ask does not trade.
        assert_eq!(e.apply(new_lim(3, 3, 3, Ask, 1500, 1)), vec![ack(3, 3)]);
    }

    pub fn partial_fill_aggressor_rests<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 4));
        assert_eq!(
            e.apply(new_lim(2, 2, 2, Bid, 1500, 10)),
            vec![ack(2, 2), fill(2, 2, 1, 1500, 4, Bid)]
        );
        // The 6-lot remainder rested as a bid: a new ask fills against it.
        assert_eq!(
            e.apply(new_lim(3, 3, 3, Ask, 1500, 6)),
            vec![ack(3, 3), fill(3, 3, 2, 1500, 6, Ask)]
        );
    }

    pub fn partial_fill_maker_remains<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 10));
        assert_eq!(
            e.apply(new_lim(2, 2, 2, Bid, 1500, 4)),
            vec![ack(2, 2), fill(2, 2, 1, 1500, 4, Bid)]
        );
        // Maker keeps its 6 remaining lots and its place in the book.
        assert_eq!(
            e.apply(new_lim(3, 3, 3, Bid, 1500, 6)),
            vec![ack(3, 3), fill(3, 3, 1, 1500, 6, Bid)]
        );
    }

    pub fn multi_level_sweep<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 5));
        e.apply(new_lim(2, 2, 2, Ask, 1501, 5));
        e.apply(new_lim(3, 3, 3, Ask, 1502, 5));
        assert_eq!(
            e.apply(new_lim(4, 4, 4, Bid, 1503, 20)),
            vec![
                ack(4, 4),
                fill(4, 4, 1, 1500, 5, Bid),
                fill(4, 4, 2, 1501, 5, Bid),
                fill(4, 4, 3, 1502, 5, Bid),
            ]
        );
        // The 5-lot remainder rested at 1503.
        assert_eq!(
            e.apply(new_lim(5, 5, 5, Ask, 1503, 5)),
            vec![ack(5, 5), fill(5, 5, 4, 1503, 5, Ask)]
        );
    }

    pub fn price_improvement<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1400, 10));
        // Aggressive bid at 1600 fills at the RESTING price, 1400.
        assert_eq!(
            e.apply(new_lim(2, 2, 2, Bid, 1600, 10)),
            vec![ack(2, 2), fill(2, 2, 1, 1400, 10, Bid)]
        );
    }

    pub fn fifo_priority<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 5));
        e.apply(new_lim(2, 2, 2, Ask, 1500, 5));
        // The older ask (id 1) fills first and fully; the newer fills the rest.
        assert_eq!(
            e.apply(new_lim(3, 3, 3, Bid, 1500, 7)),
            vec![
                ack(3, 3),
                fill(3, 3, 1, 1500, 5, Bid),
                fill(3, 3, 2, 1500, 2, Bid),
            ]
        );
    }

    pub fn cancel_resting<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Bid, 1500, 10));
        assert_eq!(
            e.apply(cancel(2, 1, 1)),
            vec![cancelled(2, 1, 10, CR::User)]
        );
        // Really gone: an ask at that price does not trade.
        assert_eq!(e.apply(new_lim(3, 2, 2, Ask, 1500, 1)), vec![ack(3, 2)]);
    }

    pub fn cancel_nonexistent<E: Engine>() {
        let mut e = eng::<E>();
        assert_eq!(
            e.apply(cancel(1, 99, 1)),
            vec![rejected(1, 99, RR::UnknownOrderId)]
        );
    }

    pub fn cancel_wrong_trader<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Bid, 1500, 10));
        assert_eq!(
            e.apply(cancel(2, 1, 2)),
            vec![rejected(2, 1, RR::WrongTrader)]
        );
        // Still owned and cancellable by the right trader.
        assert_eq!(
            e.apply(cancel(3, 1, 1)),
            vec![cancelled(3, 1, 10, CR::User)]
        );
    }

    pub fn cancel_after_partial_fill<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 10));
        e.apply(new_lim(2, 2, 2, Bid, 1500, 4));
        assert_eq!(e.apply(cancel(3, 1, 1)), vec![cancelled(3, 1, 6, CR::User)]);
    }

    pub fn ioc_partial<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 4));
        assert_eq!(
            e.apply(new_tif(2, 2, 2, Bid, 1500, 10, Tif::Ioc)),
            vec![
                ack(2, 2),
                fill(2, 2, 1, 1500, 4, Bid),
                cancelled(2, 2, 6, CR::Ioc),
            ]
        );
        // Nothing rested: a new ask does not trade.
        assert_eq!(e.apply(new_lim(3, 3, 3, Ask, 1500, 1)), vec![ack(3, 3)]);
    }

    pub fn ioc_no_cross<E: Engine>() {
        let mut e = eng::<E>();
        assert_eq!(
            e.apply(new_tif(1, 1, 1, Bid, 1500, 10, Tif::Ioc)),
            vec![ack(1, 1), cancelled(1, 1, 10, CR::Ioc)]
        );
    }

    pub fn fok_reject_leaves_book_untouched<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 4));
        assert_eq!(
            e.apply(new_tif(2, 2, 2, Bid, 1500, 10, Tif::Fok)),
            vec![rejected(2, 2, RR::FokUnfillable)]
        );
        // Maker still has its full 4 lots.
        assert_eq!(
            e.apply(new_lim(3, 3, 3, Bid, 1500, 10)),
            vec![ack(3, 3), fill(3, 3, 1, 1500, 4, Bid)]
        );
    }

    pub fn fok_fill_across_levels<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 4));
        e.apply(new_lim(2, 2, 2, Ask, 1501, 6));
        assert_eq!(
            e.apply(new_tif(3, 3, 3, Bid, 1501, 10, Tif::Fok)),
            vec![
                ack(3, 3),
                fill(3, 3, 1, 1500, 4, Bid),
                fill(3, 3, 2, 1501, 6, Bid),
            ]
        );
    }

    pub fn stp_none_allows_self_trade<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 7, Ask, 1500, 5));
        assert_eq!(
            e.apply(new_lim(2, 2, 7, Bid, 1500, 5)),
            vec![ack(2, 2), fill(2, 2, 1, 1500, 5, Bid)]
        );
    }

    pub fn stp_cancel_resting<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 7, Ask, 1500, 5)); // own
        e.apply(new_lim(2, 2, 8, Ask, 1500, 5)); // other trader
        assert_eq!(
            e.apply(new_full(
                3,
                3,
                7,
                Bid,
                1500,
                10,
                Tif::Gtc,
                Stp::CancelResting
            )),
            vec![
                ack(3, 3),
                cancelled(3, 1, 5, CR::SelfTradePrevention),
                fill(3, 3, 2, 1500, 5, Bid),
            ]
        );
        // The 5-lot remainder rested as a bid.
        assert_eq!(
            e.apply(new_lim(4, 4, 9, Ask, 1500, 5)),
            vec![ack(4, 4), fill(4, 4, 3, 1500, 5, Ask)]
        );
    }

    pub fn stp_cancel_aggressor<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 7, Ask, 1500, 5));
        assert_eq!(
            e.apply(new_full(
                2,
                2,
                7,
                Bid,
                1500,
                10,
                Tif::Gtc,
                Stp::CancelAggressor
            )),
            vec![ack(2, 2), cancelled(2, 2, 10, CR::SelfTradePrevention)]
        );
        // The resting order survived untouched.
        assert_eq!(
            e.apply(new_lim(3, 3, 8, Bid, 1500, 5)),
            vec![ack(3, 3), fill(3, 3, 1, 1500, 5, Bid)]
        );
    }

    pub fn stp_cancel_both<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 7, Ask, 1500, 5));
        assert_eq!(
            e.apply(new_full(2, 2, 7, Bid, 1500, 10, Tif::Gtc, Stp::CancelBoth)),
            vec![
                ack(2, 2),
                cancelled(2, 1, 5, CR::SelfTradePrevention),
                cancelled(2, 2, 10, CR::SelfTradePrevention),
            ]
        );
        // Book is empty on both sides.
        assert_eq!(e.apply(new_lim(3, 3, 8, Bid, 1500, 1)), vec![ack(3, 3)]);
        assert_eq!(
            e.apply(cancel(4, 1, 7)),
            vec![rejected(4, 1, RR::UnknownOrderId)]
        );
    }

    pub fn stp_middle_of_fifo<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 8, Ask, 1500, 3)); // other
        e.apply(new_lim(2, 2, 7, Ask, 1500, 4)); // own, middle of the FIFO
        e.apply(new_lim(3, 3, 9, Ask, 1500, 5)); // other
        assert_eq!(
            e.apply(new_full(
                4,
                4,
                7,
                Bid,
                1500,
                12,
                Tif::Gtc,
                Stp::CancelResting
            )),
            vec![
                ack(4, 4),
                fill(4, 4, 1, 1500, 3, Bid),
                cancelled(4, 2, 4, CR::SelfTradePrevention),
                fill(4, 4, 3, 1500, 5, Bid),
            ]
        );
    }

    pub fn duplicate_order_id<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Bid, 1500, 10));
        assert_eq!(
            e.apply(new_lim(2, 1, 1, Bid, 1400, 5)),
            vec![rejected(2, 1, RR::DuplicateOrderId)]
        );
    }

    pub fn id_reusable_after_cancel<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Bid, 1500, 10));
        e.apply(cancel(2, 1, 1));
        assert_eq!(e.apply(new_lim(3, 1, 1, Bid, 1500, 10)), vec![ack(3, 1)]);
    }

    pub fn zero_qty<E: Engine>() {
        let mut e = eng::<E>();
        assert_eq!(
            e.apply(new_lim(1, 1, 1, Bid, 1500, 0)),
            vec![rejected(1, 1, RR::ZeroQty)]
        );
    }

    pub fn price_out_of_bounds<E: Engine>() {
        let mut e = eng::<E>();
        // TEST grid: min 1000, tick 1, 1024 levels -> max 2023.
        assert_eq!(
            e.apply(new_lim(1, 1, 1, Bid, 999, 10)),
            vec![rejected(1, 1, RR::PriceOutOfBounds)]
        );
        assert_eq!(
            e.apply(new_lim(2, 2, 1, Bid, 2024, 10)),
            vec![rejected(2, 2, RR::PriceOutOfBounds)]
        );
        // Both boundaries are valid.
        assert_eq!(e.apply(new_lim(3, 3, 1, Bid, 1000, 10)), vec![ack(3, 3)]);
        assert_eq!(e.apply(new_lim(4, 4, 1, Ask, 2023, 10)), vec![ack(4, 4)]);
    }

    pub fn price_off_tick<E: Engine>() {
        let cfg = BookConfig {
            min_price: 1_000,
            tick_size: 5,
            num_levels: 100,
            max_live_orders: 64,
        };
        let mut e = E::new(cfg);
        assert_eq!(
            e.apply(new_lim(1, 1, 1, Bid, 1002, 10)),
            vec![rejected(1, 1, RR::PriceOutOfBounds)]
        );
        assert_eq!(e.apply(new_lim(2, 2, 1, Bid, 1005, 10)), vec![ack(2, 2)]);
    }

    pub fn modify_loses_priority<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 5));
        e.apply(new_lim(2, 2, 2, Ask, 1500, 5));
        // Modify id 1 to the SAME price: still loses its queue position.
        assert_eq!(
            e.apply(modify(3, 1, 1, 1500, 5)),
            vec![cancelled(3, 1, 5, CR::User), ack(3, 1)]
        );
        assert_eq!(
            e.apply(new_lim(4, 3, 3, Bid, 1500, 5)),
            vec![ack(4, 3), fill(4, 3, 2, 1500, 5, Bid)] // id 2 fills first now
        );
    }

    pub fn modify_rejects_leave_book_untouched<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Ask, 1500, 5));
        assert_eq!(
            e.apply(modify(2, 99, 1, 1500, 5)),
            vec![rejected(2, 99, RR::UnknownOrderId)]
        );
        assert_eq!(
            e.apply(modify(3, 1, 2, 1500, 5)),
            vec![rejected(3, 1, RR::WrongTrader)]
        );
        assert_eq!(
            e.apply(modify(4, 1, 1, 1500, 0)),
            vec![rejected(4, 1, RR::ZeroQty)]
        );
        assert_eq!(
            e.apply(modify(5, 1, 1, 999, 5)),
            vec![rejected(5, 1, RR::PriceOutOfBounds)]
        );
        // The order is still resting with its original quantity.
        assert_eq!(
            e.apply(new_lim(6, 2, 2, Bid, 1500, 5)),
            vec![ack(6, 2), fill(6, 2, 1, 1500, 5, Bid)]
        );
    }

    pub fn modify_can_cross<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 1, Bid, 1500, 5));
        e.apply(new_lim(2, 2, 2, Ask, 1600, 5));
        // Re-price the ask down onto the bid: the new leg trades immediately.
        assert_eq!(
            e.apply(modify(3, 2, 2, 1500, 5)),
            vec![
                cancelled(3, 2, 5, CR::User),
                ack(3, 2),
                fill(3, 2, 1, 1500, 5, Ask),
            ]
        );
    }

    pub fn fok_stp_cancel_aggressor_blocks<E: Engine>() {
        let mut e = eng::<E>();
        e.apply(new_lim(1, 1, 7, Ask, 1500, 5)); // own order at the front
        e.apply(new_lim(2, 2, 8, Ask, 1500, 10));
        // CancelAggressor: the aggressor would die at its own order before
        // reaching the 10 lots behind it -> unfillable.
        assert_eq!(
            e.apply(new_full(
                3,
                3,
                7,
                Bid,
                1500,
                8,
                Tif::Fok,
                Stp::CancelAggressor
            )),
            vec![rejected(3, 3, RR::FokUnfillable)]
        );
        // CancelResting: the own order is skipped (cancelled), the 10 lots
        // behind it are reachable -> fills.
        assert_eq!(
            e.apply(new_full(
                4,
                4,
                7,
                Bid,
                1500,
                8,
                Tif::Fok,
                Stp::CancelResting
            )),
            vec![
                ack(4, 4),
                cancelled(4, 1, 5, CR::SelfTradePrevention),
                fill(4, 4, 2, 1500, 8, Bid),
            ]
        );
    }

    pub fn arena_full<E: Engine>() {
        let cfg = BookConfig {
            min_price: 1_000,
            tick_size: 1,
            num_levels: 64,
            max_live_orders: 2,
        };
        let mut e = E::new(cfg);
        e.apply(new_lim(1, 1, 1, Bid, 1010, 1));
        e.apply(new_lim(2, 2, 1, Bid, 1011, 1));
        assert_eq!(
            e.apply(new_lim(3, 3, 1, Bid, 1012, 1)),
            vec![rejected(3, 3, RR::ArenaFull)]
        );
        // Freeing a slot makes room again.
        e.apply(cancel(4, 1, 1));
        assert_eq!(e.apply(new_lim(5, 4, 1, Bid, 1012, 1)), vec![ack(5, 4)]);
    }
}

/// Instantiate the whole shared suite for one engine type.
#[macro_export]
macro_rules! engine_suite {
    ($module:ident, $engine:ty) => {
        mod $module {
            macro_rules! case {
                ($name:ident) => {
                    #[test]
                    fn $name() {
                        tessera_tests::cases::$name::<$engine>();
                    }
                };
            }

            case!(rest_and_ack);
            case!(simple_cross_full);
            case!(partial_fill_aggressor_rests);
            case!(partial_fill_maker_remains);
            case!(multi_level_sweep);
            case!(price_improvement);
            case!(fifo_priority);
            case!(cancel_resting);
            case!(cancel_nonexistent);
            case!(cancel_wrong_trader);
            case!(cancel_after_partial_fill);
            case!(ioc_partial);
            case!(ioc_no_cross);
            case!(fok_reject_leaves_book_untouched);
            case!(fok_fill_across_levels);
            case!(stp_none_allows_self_trade);
            case!(stp_cancel_resting);
            case!(stp_cancel_aggressor);
            case!(stp_cancel_both);
            case!(stp_middle_of_fifo);
            case!(duplicate_order_id);
            case!(id_reusable_after_cancel);
            case!(zero_qty);
            case!(price_out_of_bounds);
            case!(price_off_tick);
            case!(modify_loses_priority);
            case!(modify_rejects_leave_book_untouched);
            case!(modify_can_cross);
            case!(fok_stp_cancel_aggressor_blocks);
            case!(arena_full);
        }
    };
}
