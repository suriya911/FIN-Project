//! Per-event apply latency under realistic (agent-simulated) load.
//! HDR histogram: p50 / p99 / p99.9 / p99.99 / max.

use tessera_bench::{measure_latency, print_histogram, realistic_log, BENCH_BOOK};
use tessera_core::OrderBook;

fn main() {
    let log = realistic_log(2_000_000, 42);

    // Warm-up pass (page in the arena, warm the caches and the branch
    // predictor), then the measured pass on a fresh book.
    let mut warm = OrderBook::new(BENCH_BOOK);
    let _ = measure_latency(&mut warm, &log[..500_000]);

    let mut book = OrderBook::new(BENCH_BOOK);
    let hist = measure_latency(&mut book, &log);
    print_histogram("latency: realistic 2M-event agent workload", &hist);
}
