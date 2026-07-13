//! Print microstructure stats for a simulated market (sanity-check tool).
//! Usage: simstats [seed] [events]

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(42);
    let events: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let r = tessera_sim::run(tessera_sim::SimConfig {
        seed,
        events,
        ..Default::default()
    });
    println!("seed {seed}, {} input events", r.log.len());
    println!("{}", r.stats.summary());
    println!("final state hash {:#018x}", r.final_state_hash);
}
