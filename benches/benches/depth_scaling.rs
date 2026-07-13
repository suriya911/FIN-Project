//! THE design claim: apply latency stays FLAT as the resting book grows
//! from 1k to 100k orders. Flat array + bitmap + arena means no per-order
//! or per-level data structure to traverse on the common path.
//!
//! Each run pre-fills a book to N resting orders, then measures the same
//! touch-level workload (new orders + cancels at the front of the book).

use tessera_bench::{prefill, print_histogram, BENCH_BOOK};
use tessera_core::{
    EventBuffer, InputEvent, OrderBook, OrderId, Price, Qty, SelfTradePrevention, Seq, Side,
    TimeInForce, TraderId,
};

fn touch_workload(n: usize) -> Vec<InputEvent> {
    // Alternating new/cancel pairs at the touch: the common hot path.
    let mid = BENCH_BOOK.min_price + (BENCH_BOOK.num_levels as i64) / 2;
    let mut evs = Vec::with_capacity(n);
    let mut seq = 10_000_000u64;
    let mut id = 5_000_000u64;
    while evs.len() + 2 <= n {
        seq += 1;
        id += 1;
        let bid = id % 2 == 0;
        evs.push(InputEvent::New {
            seq: Seq(seq),
            order_id: OrderId(id),
            trader: TraderId(7),
            side: if bid { Side::Bid } else { Side::Ask },
            price: Price(if bid { mid - 1 } else { mid + 1 }),
            qty: Qty(10),
            tif: TimeInForce::Gtc,
            stp: SelfTradePrevention::None,
        });
        seq += 1;
        evs.push(InputEvent::Cancel {
            seq: Seq(seq),
            order_id: OrderId(id),
            trader: TraderId(7),
        });
    }
    evs
}

fn main() {
    let work = touch_workload(1_000_000);
    for depth in [1_000u64, 10_000, 100_000] {
        let mut book = OrderBook::new(BENCH_BOOK);
        prefill(&mut book, depth);
        // Warm-up on a slice, measure the rest.
        let mut buf = EventBuffer::for_book(book.config());
        for &ev in &work[..100_000] {
            buf.clear();
            book.apply(ev, &mut buf);
        }
        let hist = tessera_bench::measure_latency(&mut book, &work[100_000..]);
        print_histogram(&format!("depth {depth:>6} resting orders"), &hist);
    }
    println!("\nThe three histograms above should be indistinguishable.");
}
