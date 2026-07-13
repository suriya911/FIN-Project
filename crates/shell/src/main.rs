//! The `tessera` CLI.
//!
//!   tessera sim    --seed 42 --events 1000000 --out session.bin
//!   tessera replay session.bin [--repeat 10]
//!   tessera tui    --seed 42
//!   tessera bench  [--events 1000000]

use std::process::ExitCode;
use tessera_shell::journal::{replay, JournalWriter};
use tessera_sim::SimConfig;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        return usage();
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "sim" => cmd_sim(rest),
        "replay" => cmd_replay(rest),
        "tui" => cmd_tui(rest),
        "bench" => cmd_bench(rest),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "tessera — a deterministic limit order book that can prove it's correct\n\n\
         USAGE:\n  \
         tessera sim    [--seed N] [--events N] [--mm N] [--momentum N] [--noise N] [--adversarial N] [--out FILE]\n  \
         tessera replay FILE [--repeat N]\n  \
         tessera tui    [--seed N]\n  \
         tessera bench  [--events N]"
    );
    ExitCode::from(2)
}

/// Tiny flag parser: `--key value` pairs plus positionals.
fn opt(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1).cloned())
}

fn opt_u64(args: &[String], key: &str, default: u64) -> u64 {
    opt(args, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn sim_config(args: &[String]) -> SimConfig {
    let d = SimConfig::default();
    SimConfig {
        seed: opt_u64(args, "--seed", d.seed),
        events: opt_u64(args, "--events", d.events as u64) as usize,
        market_makers: opt_u64(args, "--mm", d.market_makers as u64) as u32,
        momentum: opt_u64(args, "--momentum", d.momentum as u64) as u32,
        noise: opt_u64(args, "--noise", d.noise as u64) as u32,
        adversarial: opt_u64(args, "--adversarial", d.adversarial as u64) as u32,
        ..d
    }
}

fn cmd_sim(args: &[String]) -> ExitCode {
    let sc = sim_config(args);
    let t0 = std::time::Instant::now();
    let r = tessera_sim::run(sc);
    let dt = t0.elapsed();
    println!(
        "simulated {} input events in {:.2}s ({:.2}M events/s), seed {}",
        r.log.len(),
        dt.as_secs_f64(),
        r.log.len() as f64 / dt.as_secs_f64() / 1e6,
        sc.seed
    );
    println!("{}", r.stats.summary());
    println!("final state hash {:#018x}", r.final_state_hash);

    if let Some(path) = opt(args, "--out") {
        let mut w = match JournalWriter::create(&path, &sc.book) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("cannot create {path}: {e}");
                return ExitCode::FAILURE;
            }
        };
        for ev in &r.log {
            if let Err(e) = w.append(ev) {
                eprintln!("write failed: {e}");
                return ExitCode::FAILURE;
            }
        }
        match w.finish() {
            Ok(n) => println!("journal written: {path} ({n} events)"),
            Err(e) => {
                eprintln!("flush failed: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn cmd_replay(args: &[String]) -> ExitCode {
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("replay: missing journal file");
        return ExitCode::from(2);
    };
    let repeat = opt_u64(args, "--repeat", 1).max(1);

    let t0 = std::time::Instant::now();
    let first = match replay(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("replay failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "replayed {} events -> {} outputs in {:.2}s",
        first.events,
        first.outputs,
        t0.elapsed().as_secs_f64()
    );
    println!("output hash {:#018x}", first.output_hash);
    println!("state  hash {:#018x}", first.state_hash);

    for i in 1..repeat {
        let r = match replay(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("replay {i} failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        if (r.output_hash, r.state_hash) != (first.output_hash, first.state_hash) {
            eprintln!("NON-DETERMINISM on replay {i}: hashes differ!");
            return ExitCode::FAILURE;
        }
    }
    if repeat > 1 {
        println!("{repeat} replays, byte-identical outputs and state hash — deterministic.");
    }
    ExitCode::SUCCESS
}

fn cmd_tui(args: &[String]) -> ExitCode {
    // The TUI runs until 'q'; the events budget only sizes the sim config.
    let sc = sim_config(args);
    match tessera_shell::tui::run(sc) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tui error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_bench(args: &[String]) -> ExitCode {
    // Quick in-CLI measurement. The full suite is `cargo bench -p tessera-bench`.
    let events = opt_u64(args, "--events", 1_000_000) as usize;
    let sc = SimConfig {
        events,
        ..Default::default()
    };
    println!(
        "generating {events} realistic agent events (seed {})...",
        sc.seed
    );
    let log = tessera_sim::run(sc).log;

    let mut book = tessera_core::OrderBook::new(sc.book);
    let mut buf = tessera_core::EventBuffer::for_book(&sc.book);
    let mut samples: Vec<u64> = Vec::with_capacity(log.len());
    let t0 = std::time::Instant::now();
    for &ev in &log {
        buf.clear();
        let s = std::time::Instant::now();
        book.apply(ev, &mut buf);
        samples.push(s.elapsed().as_nanos() as u64);
    }
    let total = t0.elapsed();
    samples.sort_unstable();
    let pct = |q: f64| samples[((samples.len() - 1) as f64 * q) as usize];
    println!(
        "applied {} events in {:.3}s  ({:.2}M events/s)",
        log.len(),
        total.as_secs_f64(),
        log.len() as f64 / total.as_secs_f64() / 1e6
    );
    println!(
        "latency  p50 {} ns | p99 {} ns | p99.9 {} ns | p99.99 {} ns | max {} ns",
        pct(0.50),
        pct(0.99),
        pct(0.999),
        pct(0.9999),
        samples.last().copied().unwrap_or(0)
    );
    println!("(shared-machine numbers; see docs/PERF.md for methodology and caveats)");
    ExitCode::SUCCESS
}
