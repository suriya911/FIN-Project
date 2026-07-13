//! Phase 2: determinism, hashing, snapshot/restore.
//!
//! The engine is a pure state machine, so the same log must produce the
//! same outputs and the same state hash — across runs, machines, and
//! builds. These tests hold that promise mechanically.

use std::hash::Hash;
use tessera_core::hash::Fnv1a;
use tessera_core::{BookConfig, EventBuffer, InputEvent, OrderBook};
use tessera_tests::{Engine, FastBook, StreamGen, GEN_CFG};

fn gen_log(seed: u64, n: usize) -> Vec<InputEvent> {
    let mut g = StreamGen::new(seed, GEN_CFG);
    (0..n).map(|_| g.next_event()).collect()
}

/// Run a log through a fresh fast engine; fingerprint the entire output
/// stream and return it with the final state hash.
fn run(log: &[InputEvent]) -> (u64, u64) {
    let mut book = OrderBook::new(GEN_CFG);
    let mut buf = EventBuffer::for_book(&GEN_CFG);
    let mut out_hash = Fnv1a::new();
    for &ev in log {
        buf.clear();
        book.apply(ev, &mut buf);
        for e in buf.as_slice() {
            e.hash(&mut out_hash);
        }
    }
    (out_hash.finish(), book.state_hash())
}

/// Same log, many runs -> byte-identical outputs and state hash.
#[test]
fn replay_is_deterministic() {
    let log = gen_log(42, 100_000);
    let baseline = run(&log);
    for i in 0..10 {
        assert_eq!(run(&log), baseline, "divergence on run {i}");
    }
}

/// The full-fat version of the exit criterion (1M events x 100 runs).
/// Run explicitly: `cargo test --release -p tessera-tests -- --ignored`
#[test]
#[ignore = "long: 1M events x 100 runs; run with --ignored in release"]
fn replay_is_deterministic_1m_x100() {
    let log = gen_log(42, 1_000_000);
    let baseline = run(&log);
    for i in 0..100 {
        assert_eq!(run(&log), baseline, "divergence on run {i}");
    }
}

/// The fast engine and the oracle agree on the semantic state hash after
/// every event — the two implementations share only the canonical
/// definition, so agreement is meaningful.
#[test]
fn state_hash_agrees_with_reference() {
    let log = gen_log(7, 20_000);
    let mut fast = FastBook::new(GEN_CFG);
    let mut oracle = tessera_reference::ReferenceBook::new(GEN_CFG);
    for (i, &ev) in log.iter().enumerate() {
        fast.apply(ev);
        oracle.apply(ev);
        assert_eq!(
            fast.book.state_hash(),
            oracle.state_hash(),
            "state hash divergence at event {i}: {ev:?}"
        );
    }
}

/// Pin the hash of a small known book to a constant. If this ever fails,
/// either the canonical encoding changed (bump knowingly!) or the hash is
/// platform-dependent (a determinism bug). CI runs this on Linux and
/// macOS, so a platform-dependent hash cannot slip through.
#[test]
fn state_hash_pinned_value() {
    let mut e = FastBook::new(BookConfig::TEST);
    e.apply(tessera_tests::new_lim(
        1,
        10,
        3,
        tessera_core::Side::Bid,
        1500,
        25,
    ));
    e.apply(tessera_tests::new_lim(
        2,
        11,
        4,
        tessera_core::Side::Ask,
        1501,
        40,
    ));
    assert_eq!(e.book.state_hash(), 0xf043_c58d_904a_a2f0);
}

/// snapshot(N) + replay_from(N) == full_replay — for outputs AND hash.
#[test]
fn snapshot_fast_forward_equals_full_replay() {
    let log = gen_log(1234, 50_000);
    let split = 25_000;

    // Full replay.
    let mut full = OrderBook::new(GEN_CFG);
    let mut buf = EventBuffer::for_book(&GEN_CFG);
    for &ev in &log[..split] {
        buf.clear();
        full.apply(ev, &mut buf);
    }
    let snap = full.snapshot();

    // Restored book must be semantically identical right away...
    let mut restored = OrderBook::restore(&snap).expect("restore failed");
    assert_eq!(restored.state_hash(), full.state_hash());
    restored.validate().expect("restored book invalid");

    // ...and must produce identical outputs for the rest of the log.
    let mut h_full = Fnv1a::new();
    let mut h_rest = Fnv1a::new();
    let mut buf2 = EventBuffer::for_book(&GEN_CFG);
    for &ev in &log[split..] {
        buf.clear();
        buf2.clear();
        full.apply(ev, &mut buf);
        restored.apply(ev, &mut buf2);
        for e in buf.as_slice() {
            e.hash(&mut h_full);
        }
        for e in buf2.as_slice() {
            e.hash(&mut h_rest);
        }
    }
    assert_eq!(h_full.finish(), h_rest.finish());
    assert_eq!(full.state_hash(), restored.state_hash());
}

/// The generator itself must be deterministic, or every test above is
/// meaningless flake-bait.
#[test]
fn generator_is_deterministic() {
    assert_eq!(gen_log(99, 10_000), gen_log(99, 10_000));
}
