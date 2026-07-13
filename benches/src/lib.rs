//! Shared benchmark plumbing: workload builders, a latency histogram
//! wrapper, and the panicking-allocator guard used to PROVE the engine
//! does not allocate in steady state.

use hdrhistogram::Histogram;
use std::time::Instant;
use tessera_core::{BookConfig, EventBuffer, InputEvent, OrderBook};

pub const BENCH_BOOK: BookConfig = BookConfig {
    min_price: 1_000,
    tick_size: 1,
    num_levels: 4_096,
    max_live_orders: 262_144,
};

/// A realistic input log from the agent simulator (market makers,
/// momentum, noise, adversarial) — NOT `random_order()` in a loop.
pub fn realistic_log(events: usize, seed: u64) -> Vec<InputEvent> {
    tessera_sim::run(tessera_sim::SimConfig {
        seed,
        events,
        book: BENCH_BOOK,
        ..Default::default()
    })
    .log
}

/// Measure per-event apply latency into an HDR histogram.
///
/// Timing uses `Instant::now()` (clock_gettime): ~20–30ns overhead per
/// sample on this class of hardware, which inflates the LOW end of the
/// distribution. Stated rather than hidden.
pub fn measure_latency(book: &mut OrderBook, log: &[InputEvent]) -> Histogram<u64> {
    let mut hist = Histogram::<u64>::new_with_bounds(1, 1_000_000_000, 3).unwrap();
    let mut buf = EventBuffer::for_book(book.config());
    for &ev in log {
        buf.clear();
        let t0 = Instant::now();
        book.apply(ev, &mut buf);
        let ns = t0.elapsed().as_nanos() as u64;
        hist.record(ns.max(1)).unwrap();
        std::hint::black_box(buf.as_slice());
    }
    hist
}

pub fn print_histogram(label: &str, hist: &Histogram<u64>) {
    println!(
        "{label}\n  p50 {:>7} ns | p99 {:>7} ns | p99.9 {:>7} ns | p99.99 {:>8} ns | max {:>9} ns  (n={})",
        hist.value_at_quantile(0.50),
        hist.value_at_quantile(0.99),
        hist.value_at_quantile(0.999),
        hist.value_at_quantile(0.9999),
        hist.max(),
        hist.len(),
    );
}

/// Pre-fill a book with `n` resting GTC orders spread across levels away
/// from the touch, so depth-scaling runs measure a book of realistic
/// shape at a controlled size.
pub fn prefill(book: &mut OrderBook, n: u64) {
    let cfg = *book.config();
    let mut buf = EventBuffer::for_book(&cfg);
    let mid = cfg.min_price + (cfg.num_levels as i64) / 2;
    for i in 0..n {
        // Alternate sides; walk prices outward so levels stay populated.
        let bid = i % 2 == 0;
        let off = 2 + (i as i64 / 2) % 1_500;
        let price = if bid { mid - off } else { mid + off };
        buf.clear();
        book.apply(
            tessera_core::InputEvent::New {
                seq: tessera_core::Seq(i + 1),
                order_id: tessera_core::OrderId(1_000_000_000 + i),
                trader: tessera_core::TraderId(999),
                side: if bid {
                    tessera_core::Side::Bid
                } else {
                    tessera_core::Side::Ask
                },
                price: tessera_core::Price(price),
                qty: tessera_core::Qty(5),
                tif: tessera_core::TimeInForce::Gtc,
                stp: tessera_core::SelfTradePrevention::None,
            },
            &mut buf,
        );
    }
    assert_eq!(
        book.live_count() as u64,
        n,
        "prefill was consumed by crossing"
    );
}

// ---------------------------------------------------------------------
// The panicking allocator guard (armed AFTER startup allocation).
// ---------------------------------------------------------------------

pub mod alloc_guard {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, Ordering};

    pub static ARMED: AtomicBool = AtomicBool::new(false);

    /// Wraps the system allocator; panics on ANY allocation while armed.
    /// Frees stay allowed (steady state may drop, never grow).
    pub struct PanicWhenArmed;

    unsafe impl GlobalAlloc for PanicWhenArmed {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if ARMED.load(Ordering::Relaxed) {
                ARMED.store(false, Ordering::Relaxed); // let the panic itself allocate
                panic!("allocation in steady state: {} bytes", layout.size());
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    pub fn arm() {
        ARMED.store(true, Ordering::SeqCst);
    }
    pub fn disarm() {
        ARMED.store(false, Ordering::SeqCst);
    }
}
