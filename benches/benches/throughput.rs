//! Throughput (criterion). Reported LAST in the README — it is the
//! vanity metric; the latency tail is the honest one.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;
use tessera_bench::{realistic_log, BENCH_BOOK};
use tessera_core::{EventBuffer, OrderBook};

fn throughput(c: &mut Criterion) {
    let log = realistic_log(1_000_000, 42);
    let mut group = c.benchmark_group("engine");
    group.throughput(Throughput::Elements(log.len() as u64));
    group.sample_size(10);
    group.bench_function("replay_1M_realistic_events", |b| {
        b.iter(|| {
            let mut book = OrderBook::new(BENCH_BOOK);
            let mut buf = EventBuffer::for_book(&BENCH_BOOK);
            for &ev in &log {
                buf.clear();
                book.apply(ev, &mut buf);
                black_box(buf.len());
            }
            black_box(book.live_count())
        });
    });
    group.finish();
}

criterion_group!(benches, throughput);
criterion_main!(benches);
