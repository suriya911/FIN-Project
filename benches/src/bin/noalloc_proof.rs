//! PROOF of zero allocation in steady state.
//!
//! A global allocator that panics on any allocation is armed AFTER the
//! book, buffer, and input log are built. Two million events then run
//! through `apply()`. If this program prints PROOF OK, the engine did
//! not allocate once while matching — not a claim, a mechanical fact.

use tessera_bench::alloc_guard::{arm, disarm, PanicWhenArmed};
use tessera_bench::{realistic_log, BENCH_BOOK};
use tessera_core::{EventBuffer, OrderBook};

#[global_allocator]
static GUARD: PanicWhenArmed = PanicWhenArmed;

fn main() {
    // Startup: every allocation the engine will ever make happens here.
    let log = realistic_log(2_000_000, 42);
    let mut book = OrderBook::new(BENCH_BOOK);
    let mut buf = EventBuffer::for_book(&BENCH_BOOK);

    arm();
    for &ev in &log {
        buf.clear();
        book.apply(ev, &mut buf);
        std::hint::black_box(buf.len());
    }
    disarm();

    println!(
        "PROOF OK: {} events applied with the panicking allocator armed — zero allocations in steady state ({} orders still live).",
        log.len(),
        book.live_count()
    );
}
