//! Latency under a ~90% cancel-rate storm — the workload real market
//! makers actually generate, and the reason cancel is O(1) by design.

use tessera_bench::{prefill, print_histogram, BENCH_BOOK};
use tessera_core::{
    EventBuffer, InputEvent, OrderBook, OrderId, Price, Qty, SelfTradePrevention, Seq, Side,
    TimeInForce, TraderId,
};
use tessera_sim::Pcg32;

fn main() {
    let mut book = OrderBook::new(BENCH_BOOK);
    prefill(&mut book, 50_000);

    // 90% of ops cancel a random live order; 10% place a replacement.
    // Track live ids exactly so every cancel hits (pure cancel path).
    let mut rng = Pcg32::new(7);
    let mid = BENCH_BOOK.min_price + (BENCH_BOOK.num_levels as i64) / 2;
    let mut live: Vec<(u64, u32)> = (0..50_000u64).map(|i| (1_000_000_000 + i, 999)).collect();
    let mut next_id = 5_000_000u64;
    let mut seq = 10_000_000u64;
    let mut log = Vec::with_capacity(2_000_000);
    for _ in 0..2_000_000 {
        seq += 1;
        if rng.chance(90) && live.len() > 1_000 {
            let i = rng.below(live.len() as u64) as usize;
            let (id, trader) = live.swap_remove(i);
            log.push(InputEvent::Cancel {
                seq: Seq(seq),
                order_id: OrderId(id),
                trader: TraderId(trader),
            });
        } else {
            next_id += 1;
            let bid = rng.chance(50);
            let off = 2 + rng.below(1_000) as i64;
            live.push((next_id, 7));
            log.push(InputEvent::New {
                seq: Seq(seq),
                order_id: OrderId(next_id),
                trader: TraderId(7),
                side: if bid { Side::Bid } else { Side::Ask },
                price: Price(if bid { mid - off } else { mid + off }),
                qty: Qty(5),
                tif: TimeInForce::Gtc,
                stp: SelfTradePrevention::None,
            });
        }
    }

    let mut buf = EventBuffer::for_book(book.config());
    for &ev in &log[..200_000] {
        buf.clear();
        book.apply(ev, &mut buf); // warm-up
    }
    let hist = tessera_bench::measure_latency(&mut book, &log[200_000..]);
    print_histogram("cancel storm (90% cancels, 50k resting)", &hist);
}
