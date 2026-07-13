//! The 15 known-hard cases from the technical design (§12), written as
//! explicit, numbered tests. Most run BOTH engines and require identical
//! output — the differential check is stronger than any hand-written
//! expectation, and the hand-written ones are here too where they add
//! clarity.

use tessera_core::CancelReason as CR;
use tessera_core::{
    BookConfig, EventBuffer, InputEvent, OrderBook, OutputEvent, RejectReason, Side, TimeInForce,
};
use tessera_reference::ReferenceBook;
use tessera_tests::{ack, cancel, cancelled, fill, modify, new_lim, new_tif, rejected};

const CFG: BookConfig = BookConfig::TEST;

/// Run one event through both engines; assert byte-identical output and
/// clean invariants; return the shared output.
struct Pair {
    fast: OrderBook,
    buf: EventBuffer,
    oracle: ReferenceBook,
}

impl Pair {
    fn new(cfg: BookConfig) -> Self {
        Pair {
            fast: OrderBook::new(cfg),
            buf: EventBuffer::for_book(&cfg),
            oracle: ReferenceBook::new(cfg),
        }
    }

    fn apply(&mut self, ev: InputEvent) -> Vec<OutputEvent> {
        self.buf.clear();
        self.fast.apply(ev, &mut self.buf);
        let expected = self.oracle.apply(ev);
        assert_eq!(
            self.buf.as_slice(),
            expected.as_slice(),
            "engines diverged on {ev:?}"
        );
        self.fast.validate().expect("invariant violated");
        assert_eq!(
            self.fast.state_hash(),
            self.oracle.state_hash(),
            "state hash diverged"
        );
        expected
    }
}

/// 1. Aggressor exactly consumes one full level and stops: the bitmap bit
///    must clear and `best` must rescan to the next level.
#[test]
fn case_01_exact_level_consumption_rescans_best() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 10));
    p.apply(new_lim(2, 2, 2, Side::Ask, 1501, 10));
    p.apply(new_lim(3, 3, 3, Side::Bid, 1500, 10)); // eats 1500 exactly
    assert_eq!(p.fast.best_ask(), Some(tessera_core::Price(1501)));
}

/// 2. Aggressor sweeps three levels and rests the remainder at a fourth.
#[test]
fn case_02_sweep_three_levels_rest_at_fourth() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 5));
    p.apply(new_lim(2, 2, 2, Side::Ask, 1501, 5));
    p.apply(new_lim(3, 3, 3, Side::Ask, 1502, 5));
    let out = p.apply(new_lim(4, 4, 4, Side::Bid, 1503, 20));
    assert_eq!(
        out.iter()
            .filter(|e| matches!(e, OutputEvent::Fill { .. }))
            .count(),
        3
    );
    assert_eq!(p.fast.best_bid(), Some(tessera_core::Price(1503)));
    assert_eq!(p.fast.best_ask(), None);
}

/// 3. Cancel the only order at the best level: best must rescan.
#[test]
fn case_03_cancel_best_level_rescans() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Bid, 1600, 5));
    p.apply(new_lim(2, 2, 1, Side::Bid, 1550, 5));
    p.apply(cancel(3, 1, 1));
    assert_eq!(p.fast.best_bid(), Some(tessera_core::Price(1550)));
}

/// 4. Cancel the only order in the ENTIRE book: best must become none.
#[test]
fn case_04_cancel_last_order_empties_book() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Bid, 1600, 5));
    p.apply(cancel(2, 1, 1));
    assert_eq!(p.fast.best_bid(), None);
    assert_eq!(p.fast.best_ask(), None);
    assert_eq!(p.fast.live_count(), 0);
}

/// 5. FOK that can ALMOST fill: reject, and the book must be
///    byte-identical to before (state-hash checked).
#[test]
fn case_05_fok_almost_fill_leaves_book_untouched() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 4));
    p.apply(new_lim(2, 2, 2, Side::Ask, 1501, 5));
    let before = p.fast.state_hash();
    let out = p.apply(new_tif(3, 3, 3, Side::Bid, 1501, 10, TimeInForce::Fok));
    assert_eq!(out, vec![rejected(3, 3, RejectReason::FokUnfillable)]);
    assert_eq!(p.fast.state_hash(), before);
}

/// 6. IOC that partially fills: fills, then Cancelled{Ioc} for the rest.
#[test]
fn case_06_ioc_partial_fill_then_cancel() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 4));
    let out = p.apply(new_tif(2, 2, 2, Side::Bid, 1500, 10, TimeInForce::Ioc));
    assert_eq!(
        out,
        vec![
            ack(2, 2),
            fill(2, 2, 1, 1500, 4, Side::Bid),
            cancelled(2, 2, 6, CR::Ioc),
        ]
    );
}

/// 7. Self-trade against your own order in the MIDDLE of a FIFO: the
///    doubly-linked list must splice correctly around it.
#[test]
fn case_07_self_trade_mid_fifo_splice() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 8, Side::Ask, 1500, 3));
    p.apply(new_lim(2, 2, 7, Side::Ask, 1500, 4)); // own, middle
    p.apply(new_lim(3, 3, 9, Side::Ask, 1500, 5));
    p.apply(tessera_tests::new_full(
        4,
        4,
        7,
        Side::Bid,
        1500,
        12,
        TimeInForce::Gtc,
        tessera_core::SelfTradePrevention::CancelResting,
    ));
    // After: 3 filled + 4 STP-cancelled + 5 filled, 4 lots rest as bid.
    assert_eq!(p.fast.best_bid(), Some(tessera_core::Price(1500)));
    assert_eq!(p.fast.live_count(), 1);
}

/// 8. Duplicate OrderId while the first is still live: reject.
#[test]
fn case_08_duplicate_live_id_rejected() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 7, 1, Side::Bid, 1500, 5));
    let out = p.apply(new_lim(2, 7, 2, Side::Ask, 1600, 5));
    assert_eq!(out, vec![rejected(2, 7, RejectReason::DuplicateOrderId)]);
}

/// 9. Cancel a non-existent order, and cancel an already-filled order:
///    both reject with UnknownOrderId.
#[test]
fn case_09_cancel_nonexistent_and_filled() {
    let mut p = Pair::new(CFG);
    assert_eq!(
        p.apply(cancel(1, 42, 1)),
        vec![rejected(1, 42, RejectReason::UnknownOrderId)]
    );
    p.apply(new_lim(2, 1, 1, Side::Ask, 1500, 5));
    p.apply(new_lim(3, 2, 2, Side::Bid, 1500, 5)); // fills order 1 away
    assert_eq!(
        p.apply(cancel(4, 1, 1)),
        vec![rejected(4, 1, RejectReason::UnknownOrderId)]
    );
}

/// 10. Arena exhaustion: reject cleanly, corrupt nothing, recover after
///     a free.
#[test]
fn case_10_arena_exhaustion_clean_reject() {
    let cfg = BookConfig {
        min_price: 1_000,
        tick_size: 1,
        num_levels: 64,
        max_live_orders: 3,
    };
    let mut p = Pair::new(cfg);
    p.apply(new_lim(1, 1, 1, Side::Bid, 1010, 1));
    p.apply(new_lim(2, 2, 1, Side::Bid, 1011, 1));
    p.apply(new_lim(3, 3, 1, Side::Bid, 1012, 1));
    let out = p.apply(new_lim(4, 4, 1, Side::Bid, 1013, 1));
    assert_eq!(out, vec![rejected(4, 4, RejectReason::ArenaFull)]);
    // Book still fully functional.
    p.apply(cancel(5, 2, 1));
    assert_eq!(
        p.apply(new_lim(6, 5, 1, Side::Bid, 1013, 1)),
        vec![ack(6, 5)]
    );
}

/// 11. A fill that takes remaining to EXACTLY zero: the maker must be
///     unlinked — no zero-qty ghost left in the FIFO.
#[test]
fn case_11_exact_zero_fill_no_ghost() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 5));
    p.apply(new_lim(2, 2, 2, Side::Bid, 1500, 5)); // exact
    assert_eq!(p.fast.live_count(), 0);
    // Its id is gone from the index too.
    assert_eq!(
        p.apply(cancel(3, 1, 1)),
        vec![rejected(3, 1, RejectReason::UnknownOrderId)]
    );
}

/// 12. Modify to the SAME price: still loses time priority. Documented,
///     deliberate.
#[test]
fn case_12_modify_same_price_loses_priority() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 1, Side::Ask, 1500, 5));
    p.apply(new_lim(2, 2, 2, Side::Ask, 1500, 5));
    p.apply(modify(3, 1, 1, 1500, 5));
    let out = p.apply(new_lim(4, 3, 3, Side::Bid, 1500, 5));
    assert_eq!(out, vec![ack(4, 3), fill(4, 3, 2, 1500, 5, Side::Bid)]);
}

/// 13. Prices exactly at min_price and max_price: the boundaries of the
///     flat array must accept orders; one tick beyond must reject.
#[test]
fn case_13_price_grid_boundaries() {
    let mut p = Pair::new(CFG);
    let max = CFG.max_price().0;
    assert_eq!(
        p.apply(new_lim(1, 1, 1, Side::Bid, CFG.min_price, 5)),
        vec![ack(1, 1)]
    );
    assert_eq!(
        p.apply(new_lim(2, 2, 1, Side::Ask, max, 5)),
        vec![ack(2, 2)]
    );
    assert_eq!(
        p.apply(new_lim(3, 3, 1, Side::Bid, CFG.min_price - 1, 5)),
        vec![rejected(3, 3, RejectReason::PriceOutOfBounds)]
    );
    assert_eq!(
        p.apply(new_lim(4, 4, 1, Side::Ask, max + 1, 5)),
        vec![rejected(4, 4, RejectReason::PriceOutOfBounds)]
    );
    // And they actually trade across the full grid width.
    let out = p.apply(new_lim(5, 5, 2, Side::Bid, max, 5));
    assert_eq!(out, vec![ack(5, 5), fill(5, 5, 2, max, 5, Side::Bid)]);
}

/// 14. A single level with 10,000 orders in the FIFO: the walk must stay
///     linear (finishes instantly) and fill in exact arrival order.
#[test]
fn case_14_deep_fifo_single_level() {
    let n: u64 = 10_000;
    let cfg = BookConfig {
        min_price: 1_000,
        tick_size: 1,
        num_levels: 16,
        max_live_orders: n as u32 + 8,
    };
    let mut p = Pair::new(cfg);
    for i in 0..n {
        p.apply(new_lim(i + 1, i + 1, 1, Side::Ask, 1005, 1));
    }
    // One taker sweeps the whole level; makers must fill 1,2,3,... in order.
    let out = p.apply(new_lim(n + 1, n + 1, 2, Side::Bid, 1005, n));
    let makers: Vec<u64> = out
        .iter()
        .filter_map(|e| match e {
            OutputEvent::Fill { maker, .. } => Some(maker.0),
            _ => None,
        })
        .collect();
    assert_eq!(makers.len(), n as usize);
    assert!(
        makers.windows(2).all(|w| w[0] < w[1]),
        "FIFO order violated"
    );
    assert_eq!(p.fast.live_count(), 0);
}

/// 15. Bitmap word boundary: best transitions 63 -> 64 and 64 -> 63
///     (levels sitting in adjacent u64 words), both sides.
#[test]
fn case_15_bitmap_word_boundary() {
    // num_levels 128, tick 1: level i = price min+i. Levels 63 and 64
    // straddle the first word boundary.
    let cfg = BookConfig {
        min_price: 0,
        tick_size: 1,
        num_levels: 128,
        max_live_orders: 16,
    };
    let mut p = Pair::new(cfg);
    // Asks at 63 and 64: best is 63; consume it; best must cross to 64.
    p.apply(new_lim(1, 1, 1, Side::Ask, 63, 1));
    p.apply(new_lim(2, 2, 1, Side::Ask, 64, 1));
    p.apply(new_lim(3, 3, 2, Side::Bid, 63, 1));
    assert_eq!(p.fast.best_ask(), Some(tessera_core::Price(64)));
    // Bids at 64 and 63: best is 64; consume it; best must cross to 63.
    p.apply(new_lim(4, 4, 1, Side::Bid, 63, 1));
    p.apply(new_lim(5, 5, 1, Side::Bid, 64, 1));
    p.apply(new_lim(6, 6, 2, Side::Ask, 64, 1));
    assert_eq!(p.fast.best_bid(), Some(tessera_core::Price(63)));
}

/// 16 (added during design review): FOK × STP interaction — with
///     CancelAggressor the taker cannot reach quantity behind its own
///     resting order, so the FOK pre-check must count only what sits
///     AHEAD of the first own order.
#[test]
fn case_16_fok_stp_interaction() {
    let mut p = Pair::new(CFG);
    p.apply(new_lim(1, 1, 7, Side::Ask, 1500, 5)); // own, front of queue
    p.apply(new_lim(2, 2, 8, Side::Ask, 1500, 10));
    let out = p.apply(tessera_tests::new_full(
        3,
        3,
        7,
        Side::Bid,
        1500,
        8,
        TimeInForce::Fok,
        tessera_core::SelfTradePrevention::CancelAggressor,
    ));
    assert_eq!(out, vec![rejected(3, 3, RejectReason::FokUnfillable)]);
}
