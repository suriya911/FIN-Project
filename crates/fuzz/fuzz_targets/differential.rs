//! THE IMPORTANT ONE: the differential fuzz target.
//!
//! libFuzzer feeds arbitrary byte strings; `arbitrary` shapes them into
//! structured operation streams; both engines consume the identical
//! stream. Any difference in output events, any invariant violation, or
//! any state-hash mismatch panics — and libFuzzer minimizes the input to
//! a tiny reproducer that goes straight into tests/regressions/.
//!
//! Run: `cargo +nightly fuzz run differential --fuzz-dir crates/fuzz`
//! Overnight: append `-- -max_total_time=28800`

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tessera_core::{
    BookConfig, EventBuffer, InputEvent, OrderBook, OrderId, Price, Qty, SelfTradePrevention,
    Seq, TimeInForce, TraderId,
};
use tessera_reference::ReferenceBook;

/// Tiny config: capacity limits, grid edges, and level churn all within
/// reach of short inputs.
const CFG: BookConfig = BookConfig {
    min_price: 100,
    tick_size: 1,
    num_levels: 128,
    max_live_orders: 64,
};

/// One structured operation. Small pools on purpose: 8-bit ids collide
/// and get reused, 2-bit traders self-trade constantly.
#[derive(Arbitrary, Debug)]
enum RawOp {
    New {
        id: u8,
        trader: u8,
        bid: bool,
        price_off: i16,
        qty: u64,
        tif: u8,
        stp: u8,
    },
    Cancel {
        id: u8,
        trader: u8,
    },
    Modify {
        id: u8,
        trader: u8,
        price_off: i16,
        qty: u64,
    },
}

#[derive(Arbitrary, Debug)]
struct Ops(Vec<RawOp>);

impl RawOp {
    fn to_event(&self, seq: u64) -> InputEvent {
        let price = |off: i16| Price(CFG.min_price + off as i64);
        let trader = |t: u8| TraderId((t % 4) as u32);
        match *self {
            RawOp::New {
                id,
                trader: t,
                bid,
                price_off,
                qty,
                tif,
                stp,
            } => InputEvent::New {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: trader(t),
                side: if bid {
                    tessera_core::Side::Bid
                } else {
                    tessera_core::Side::Ask
                },
                price: price(price_off),
                qty: Qty(qty),
                tif: match tif % 3 {
                    0 => TimeInForce::Gtc,
                    1 => TimeInForce::Ioc,
                    _ => TimeInForce::Fok,
                },
                stp: SelfTradePrevention::from_u8(stp % 4),
            },
            RawOp::Cancel { id, trader: t } => InputEvent::Cancel {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: trader(t),
            },
            RawOp::Modify {
                id,
                trader: t,
                price_off,
                qty,
            } => InputEvent::Modify {
                seq: Seq(seq),
                order_id: OrderId(id as u64),
                trader: trader(t),
                new_price: price(price_off),
                new_qty: Qty(qty),
            },
        }
    }
}

fuzz_target!(|ops: Ops| {
    let mut fast = OrderBook::new(CFG);
    let mut buf = EventBuffer::for_book(&CFG);
    let mut oracle = ReferenceBook::new(CFG);

    for (i, raw) in ops.0.iter().enumerate() {
        let ev = raw.to_event(i as u64);

        buf.clear();
        fast.apply(ev, &mut buf);
        let expected = oracle.apply(ev);

        assert_eq!(
            buf.as_slice(),
            expected.as_slice(),
            "DIVERGENCE at event {i}: {ev:?}\n  fast:   {:?}\n  oracle: {:?}",
            buf.as_slice(),
            expected
        );
        if let Err(violation) = fast.validate() {
            panic!("INVARIANT VIOLATION at event {i}: {violation} after {ev:?}");
        }
        assert_eq!(
            fast.state_hash(),
            oracle.state_hash(),
            "STATE HASH DIVERGENCE at event {i} after {ev:?}"
        );
    }
});
