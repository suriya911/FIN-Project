//! Phase 3: property-based differential testing.
//!
//! proptest generates structured operation streams, runs them through
//! BOTH engines, and demands byte-identical output streams plus a clean
//! invariant check and matching state hashes after every event. On
//! failure proptest shrinks to a minimal reproducer — each one that finds
//! a bug gets committed under tests/regressions/.

use proptest::prelude::*;
use tessera_core::{
    BookConfig, CancelReason, InputEvent, OrderId, OutputEvent, Price, Qty, RejectReason,
    SelfTradePrevention, Seq, Side, TimeInForce, TraderId,
};
use tessera_tests::{Engine, FastBook};

/// Small config so capacity, boundaries, and level-emptying all happen
/// inside short shrunk sequences.
const CFG: BookConfig = BookConfig {
    min_price: 100,
    tick_size: 1,
    num_levels: 128,
    max_live_orders: 64,
};

/// A structured raw operation. Small id/trader pools force collisions,
/// duplicates, reuse, and self-trades; price offsets straddle the grid
/// edges; quantities include zero, exact-fill sizes, and u64::MAX.
#[derive(Debug, Clone)]
enum RawOp {
    New {
        id: u16,
        trader: u8,
        bid: bool,
        price_off: i16,
        qty: u64,
        tif: u8,
        stp: u8,
    },
    Cancel {
        id: u16,
        trader: u8,
    },
    Modify {
        id: u16,
        trader: u8,
        price_off: i16,
        qty: u64,
    },
}

impl RawOp {
    fn to_event(&self, seq: u64) -> InputEvent {
        // Map the raw price offset onto [min-2, max+2] so a slice of the
        // range is out of bounds on each side.
        let price = |off: i16| Price(CFG.min_price + off as i64);
        match *self {
            RawOp::New {
                id,
                trader,
                bid,
                price_off,
                qty,
                tif,
                stp,
            } => InputEvent::New {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: TraderId(trader as u32),
                side: if bid { Side::Bid } else { Side::Ask },
                price: price(price_off),
                qty: Qty(qty),
                tif: match tif % 3 {
                    0 => TimeInForce::Gtc,
                    1 => TimeInForce::Ioc,
                    _ => TimeInForce::Fok,
                },
                stp: SelfTradePrevention::from_u8(stp % 4),
            },
            RawOp::Cancel { id, trader } => InputEvent::Cancel {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: TraderId(trader as u32),
            },
            RawOp::Modify {
                id,
                trader,
                price_off,
                qty,
            } => InputEvent::Modify {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: TraderId(trader as u32),
                new_price: price(price_off),
                new_qty: Qty(qty),
            },
        }
    }
}

fn qty_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        8 => 1u64..50,
        2 => 1u64..5_000,
        1 => Just(0u64),
        1 => Just(u64::MAX),
        1 => Just(u64::MAX / 2),
    ]
}

fn raw_op() -> impl Strategy<Value = RawOp> {
    let id = 0u16..24; // tiny pool: collisions and reuse are the norm
    let trader = 0u8..4; // tiny pool: self-trades are the norm
    let price_off = -2i16..(CFG.num_levels as i16 + 2);
    prop_oneof![
        5 => (id.clone(), trader.clone(), any::<bool>(), price_off.clone(), qty_strategy(),
              any::<u8>(), any::<u8>())
            .prop_map(|(id, trader, bid, price_off, qty, tif, stp)| RawOp::New {
                id, trader, bid, price_off, qty, tif, stp,
            }),
        4 => (id.clone(), trader.clone()).prop_map(|(id, trader)| RawOp::Cancel { id, trader }),
        1 => (id, trader, price_off, qty_strategy())
            .prop_map(|(id, trader, price_off, qty)| RawOp::Modify { id, trader, price_off, qty }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        ..ProptestConfig::default()
    })]

    /// THE differential property: the two engines must be observationally
    /// identical, event by event, and the fast engine's invariants and
    /// canonical hash must hold after every step.
    #[test]
    fn engines_agree(ops in proptest::collection::vec(raw_op(), 1..300)) {
        let mut fast = tessera_core::OrderBook::new(CFG);
        let mut buf = tessera_core::EventBuffer::for_book(&CFG);
        let mut oracle = tessera_reference::ReferenceBook::new(CFG);

        for (i, op) in ops.iter().enumerate() {
            let ev = op.to_event(i as u64);
            buf.clear();
            fast.apply(ev, &mut buf);
            let expected = oracle.apply(ev);
            prop_assert_eq!(
                buf.as_slice(), expected.as_slice(),
                "output divergence at event {}: {:?}", i, ev
            );
            if let Err(violation) = fast.validate() {
                return Err(TestCaseError::fail(format!(
                    "invariant violated at event {i}: {violation} after {ev:?}"
                )));
            }
            prop_assert_eq!(
                fast.state_hash(), oracle.state_hash(),
                "state hash divergence at event {}: {:?}", i, ev
            );
        }
    }

    /// Conservation (I2): for every order, quantity in == quantity out.
    /// original == filled + cancelled-remainder + still-resting.
    #[test]
    fn conservation(ops in proptest::collection::vec(raw_op(), 1..300)) {
        use std::collections::HashMap;
        let mut fast = FastBook::new(CFG);

        // Ledger per (submission generation of an) order id. Reused ids
        // start a fresh generation on each accepted New/Modify leg.
        #[derive(Default, Clone, Copy)]
        struct Ledger { submitted: u128, filled: u128, cancelled_rem: u128 }
        let mut ledgers: HashMap<u64, Ledger> = HashMap::new();
        let mut submitted_qty: HashMap<u64, u64> = HashMap::new();

        for (i, op) in ops.iter().enumerate() {
            let ev = op.to_event(i as u64);
            // Remember what each New/Modify *asked for*, keyed by id.
            match ev {
                InputEvent::New { order_id, qty, .. } => {
                    submitted_qty.insert(order_id.0, qty.0);
                }
                InputEvent::Modify { order_id, new_qty, .. } => {
                    submitted_qty.insert(order_id.0, new_qty.0);
                }
                InputEvent::Cancel { .. } => {}
            }
            for out in fast.apply(ev) {
                match out {
                    OutputEvent::Ack { order_id, .. } => {
                        let l = ledgers.entry(order_id.0).or_default();
                        l.submitted += submitted_qty[&order_id.0] as u128;
                    }
                    OutputEvent::Fill { taker, maker, qty, .. } => {
                        ledgers.entry(taker.0).or_default().filled += qty.0 as u128;
                        ledgers.entry(maker.0).or_default().filled += qty.0 as u128;
                    }
                    OutputEvent::Cancelled { order_id, remaining, reason, .. } => {
                        // The cancel leg of a Modify is followed by a fresh
                        // Ack for the same id; both sides of the ledger grow.
                        let _ = reason;
                        ledgers.entry(order_id.0).or_default().cancelled_rem +=
                            remaining.0 as u128;
                    }
                    OutputEvent::Rejected { .. } => {}
                }
            }
        }

        // Whatever is still resting closes each ledger.
        let snap = fast.book.snapshot();
        for o in &snap.orders {
            ledgers.entry(o.order_id).or_default().cancelled_rem += o.remaining as u128;
        }

        for (id, l) in ledgers {
            prop_assert_eq!(
                l.submitted, l.filled + l.cancelled_rem,
                "conservation violated for order id {}: submitted {} != filled {} + out {}",
                id, l.submitted, l.filled, l.cancelled_rem
            );
        }
    }

    /// The book never crosses, observed through the public API after
    /// every event (belt to validate()'s suspenders).
    #[test]
    fn book_never_crosses(ops in proptest::collection::vec(raw_op(), 1..200)) {
        let mut fast = FastBook::new(CFG);
        for (i, op) in ops.iter().enumerate() {
            fast.apply(op.to_event(i as u64));
            if let (Some(bb), Some(ba)) = (fast.book.best_bid(), fast.book.best_ask()) {
                prop_assert!(bb < ba, "crossed book after event {}: bid {:?} >= ask {:?}",
                             i, bb, ba);
            }
        }
    }

    /// FIFO priority: N same-price resting orders fill strictly in
    /// arrival order under any sequence of takers.
    #[test]
    fn fifo_priority_holds(
        makers in 2usize..12,
        taker_qtys in proptest::collection::vec(1u64..40, 1..8),
    ) {
        let mut fast = FastBook::new(CFG);
        let price = CFG.min_price + 50;
        let mut seq = 0u64;
        for m in 0..makers {
            seq += 1;
            fast.apply(tessera_tests::new_lim(seq, m as u64 + 1, 1, Side::Ask, price, 10));
        }
        let mut last_filled_maker = 0u64;
        for (t, q) in taker_qtys.iter().enumerate() {
            seq += 1;
            let out = fast.apply(tessera_tests::new_tif(
                seq, 1000 + t as u64, 2, Side::Bid, price, *q, TimeInForce::Ioc,
            ));
            for ev in out {
                if let OutputEvent::Fill { maker, .. } = ev {
                    prop_assert!(
                        maker.0 >= last_filled_maker,
                        "maker {} filled after maker {}", maker.0, last_filled_maker
                    );
                    last_filled_maker = maker.0;
                }
            }
        }
    }

    /// A rejected FOK leaves the book byte-identical (hash-checked).
    #[test]
    fn fok_reject_is_a_pure_noop(
        ops in proptest::collection::vec(raw_op(), 1..100),
        fok_qty in 1u64..10_000,
        fok_off in -2i16..(CFG.num_levels as i16 + 2),
        bid in any::<bool>(),
    ) {
        let mut fast = FastBook::new(CFG);
        for (i, op) in ops.iter().enumerate() {
            fast.apply(op.to_event(i as u64));
        }
        let before = fast.book.state_hash();
        let out = fast.apply(InputEvent::New {
            seq: Seq(9_999),
            order_id: OrderId(55_555),
            trader: TraderId(9),
            side: if bid { Side::Bid } else { Side::Ask },
            price: Price(CFG.min_price + fok_off as i64),
            qty: Qty(fok_qty),
            tif: TimeInForce::Fok,
            stp: SelfTradePrevention::None,
        });
        if out.iter().any(|e| matches!(
            e,
            OutputEvent::Rejected { reason: RejectReason::FokUnfillable, .. }
        )) {
            prop_assert_eq!(out.len(), 1, "reject must be the only event");
            prop_assert_eq!(fast.book.state_hash(), before, "rejected FOK mutated the book");
        }
    }

    /// IOC never rests: after any IOC, its id is unknown to Cancel.
    #[test]
    fn ioc_never_rests(
        ops in proptest::collection::vec(raw_op(), 1..100),
        qty in 1u64..10_000,
        off in 0i16..(CFG.num_levels as i16),
        bid in any::<bool>(),
    ) {
        let mut fast = FastBook::new(CFG);
        for (i, op) in ops.iter().enumerate() {
            fast.apply(op.to_event(i as u64));
        }
        fast.apply(InputEvent::New {
            seq: Seq(9_998),
            order_id: OrderId(44_444),
            trader: TraderId(9),
            side: if bid { Side::Bid } else { Side::Ask },
            price: Price(CFG.min_price + off as i64),
            qty: Qty(qty),
            tif: TimeInForce::Ioc,
            stp: SelfTradePrevention::None,
        });
        let out = fast.apply(tessera_tests::cancel(9_999, 44_444, 9));
        prop_assert_eq!(
            out,
            vec![tessera_tests::rejected(9_999, 44_444, RejectReason::UnknownOrderId)]
        );
    }
}

/// Silence the "unused import" pedantry if reasons stay unread above.
#[allow(unused)]
fn _keep(_: CancelReason) {}
