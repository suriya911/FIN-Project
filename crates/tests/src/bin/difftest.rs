//! Long-running differential driver on the STABLE toolchain.
//!
//! Complements the libFuzzer target (which needs nightly): seeded
//! StreamGen streams through both engines, byte-compared on every event,
//! invariants checked, hashes cross-checked. Fully deterministic — a
//! reported seed reproduces the failure exactly.
//!
//! Usage:
//!   difftest [--seeds A..B] [--events N]
//!   difftest --minutes M          # run seeds from 0 until M minutes pass
//!
//! Exit code 0 = no divergence. Any divergence prints the seed, event
//! index, and both output streams, then exits 1.

use std::time::Instant;
use tessera_core::{BookConfig, EventBuffer, OrderBook};
use tessera_reference::ReferenceBook;
use tessera_tests::StreamGen;

/// A few book shapes so capacity pressure, tick alignment, and grid width
/// all vary. Config 1's tick_size=3 exercises off-tick rejection; config
/// 2's tiny arena exercises ArenaFull constantly.
const CONFIGS: [BookConfig; 3] = [
    BookConfig {
        min_price: 1_000,
        tick_size: 1,
        num_levels: 256,
        max_live_orders: 512,
    },
    BookConfig {
        min_price: -500, // negative prices are legal (spreads, rates)
        tick_size: 3,
        num_levels: 128,
        max_live_orders: 128,
    },
    BookConfig {
        min_price: 0,
        tick_size: 1,
        num_levels: 65,
        max_live_orders: 16,
    },
];

fn run_seed(seed: u64, events: usize) -> Result<(), String> {
    for (ci, cfg) in CONFIGS.iter().enumerate() {
        let mut gen = StreamGen::new(seed ^ (ci as u64) << 32, *cfg);
        let mut fast = OrderBook::new(*cfg);
        let mut buf = EventBuffer::for_book(cfg);
        let mut oracle = ReferenceBook::new(*cfg);

        for i in 0..events {
            let ev = gen.next_event();
            buf.clear();
            fast.apply(ev, &mut buf);
            let expected = oracle.apply(ev);
            if buf.as_slice() != expected.as_slice() {
                return Err(format!(
                    "OUTPUT DIVERGENCE seed={seed} cfg={ci} event={i}\n  input:  {ev:?}\n  fast:   {:?}\n  oracle: {expected:?}",
                    buf.as_slice()
                ));
            }
            // Invariants every event; hash cross-check periodically (the
            // oracle's hash walk is O(levels x orders) and would dominate).
            if let Err(v) = fast.validate() {
                return Err(format!(
                    "INVARIANT VIOLATION seed={seed} cfg={ci} event={i}: {v}\n  input: {ev:?}"
                ));
            }
            if i % 1024 == 0 && fast.state_hash() != oracle.state_hash() {
                return Err(format!(
                    "STATE HASH DIVERGENCE seed={seed} cfg={ci} event={i} after {ev:?}"
                ));
            }
        }
        if fast.state_hash() != oracle.state_hash() {
            return Err(format!("FINAL STATE HASH DIVERGENCE seed={seed} cfg={ci}"));
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut seeds = 0u64..1_000;
    let mut events = 20_000usize;
    let mut minutes: Option<u64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seeds" => {
                let (a, b) = args[i + 1].split_once("..").expect("--seeds A..B");
                seeds = a.parse().unwrap()..b.parse().unwrap();
                i += 2;
            }
            "--events" => {
                events = args[i + 1].parse().unwrap();
                i += 2;
            }
            "--minutes" => {
                minutes = Some(args[i + 1].parse().unwrap());
                seeds = 0..u64::MAX;
                i += 2;
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
    }

    let start = Instant::now();
    let mut done = 0u64;
    for seed in seeds {
        if let Some(m) = minutes {
            if start.elapsed().as_secs() >= m * 60 {
                break;
            }
        }
        if let Err(report) = run_seed(seed, events) {
            eprintln!("{report}");
            std::process::exit(1);
        }
        done += 1;
        if done % 50 == 0 {
            eprintln!(
                "[difftest] {done} seeds x {events} events x {} configs clean ({:.0}s)",
                CONFIGS.len(),
                start.elapsed().as_secs_f64()
            );
        }
    }
    println!(
        "difftest: {} seeds x {} events x {} configs — ZERO divergences in {:.0}s",
        done,
        events,
        CONFIGS.len(),
        start.elapsed().as_secs_f64()
    );
}
