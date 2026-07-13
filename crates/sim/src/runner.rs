//! The simulation runner: a geometric-Brownian reference price, a crowd
//! of agents, one engine, and honest microstructure statistics.
//!
//! Fully deterministic: one PCG32 stream drives everything, agents act in
//! a fixed order, and the runner assigns sequence numbers — so the same
//! seed produces a byte-identical input log every time.

use crate::adversarial::Adversarial;
use crate::agent::{Agent, BookView, Intent};
use crate::market_maker::MarketMaker;
use crate::momentum::Momentum;
use crate::noise::Noise;
use crate::rng::Pcg32;
use tessera_core::{BookConfig, EventBuffer, InputEvent, OrderBook, OutputEvent, Price, TraderId};

#[derive(Copy, Clone, Debug)]
pub struct SimConfig {
    pub seed: u64,
    /// Stop at the end of the first tick where the log reaches this size
    /// (whole ticks only, so the log can slightly exceed it).
    pub events: usize,
    pub book: BookConfig,
    pub market_makers: u32,
    pub momentum: u32,
    pub noise: u32,
    pub adversarial: u32,
    /// GBM volatility per tick of sim time (float — allowed in the sim).
    pub sigma: f64,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            seed: 42,
            events: 1_000_000,
            book: BookConfig {
                min_price: 1_000,
                tick_size: 1,
                num_levels: 4_096,
                max_live_orders: 65_536,
            },
            market_makers: 20,
            momentum: 5,
            noise: 10,
            adversarial: 2,
            sigma: 0.0008,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub inputs: u64,
    pub news: u64,
    pub cancels: u64,
    pub modifies: u64,
    pub acks: u64,
    pub fills: u64,
    pub cancelled_events: u64,
    pub rejects: u64,
    pub traded_qty: u128,
    pub depth_samples: u64,
    pub depth_sum: u128,
    pub spread_samples: u64,
    pub spread_sum: u128,
}

impl Stats {
    /// Cancels submitted per new order — the classic "cancel rate".
    pub fn cancel_rate(&self) -> f64 {
        self.cancels as f64 / self.news.max(1) as f64
    }
    /// New orders per fill event — the order-to-trade ratio.
    pub fn order_to_trade(&self) -> f64 {
        self.news as f64 / self.fills.max(1) as f64
    }
    pub fn mean_depth(&self) -> f64 {
        self.depth_sum as f64 / self.depth_samples.max(1) as f64
    }
    pub fn mean_spread(&self) -> f64 {
        self.spread_sum as f64 / self.spread_samples.max(1) as f64
    }

    pub fn summary(&self) -> String {
        format!(
            "inputs        {:>12}\n\
             new orders    {:>12}\n\
             cancels       {:>12}   cancel rate    {:>8.1}% of news\n\
             modifies      {:>12}\n\
             fills         {:>12}   order-to-trade {:>8.1} : 1\n\
             rejects       {:>12}\n\
             traded lots   {:>12}\n\
             mean depth    {:>12.0} resting orders\n\
             mean spread   {:>12.2} ticks",
            self.inputs,
            self.news,
            self.cancels,
            100.0 * self.cancel_rate(),
            self.modifies,
            self.fills,
            self.order_to_trade(),
            self.rejects,
            self.traded_qty,
            self.mean_depth(),
            self.mean_spread(),
        )
    }
}

pub struct SimResult {
    pub log: Vec<InputEvent>,
    pub stats: Stats,
    pub final_state_hash: u64,
}

/// A steppable simulation: the TUI drives it one tick at a time, `run`
/// drives it to completion. One tick = the price process advances and
/// every agent acts once.
pub struct Sim {
    sc: SimConfig,
    rng: Pcg32,
    agents: Vec<Box<dyn Agent>>,
    ref_f: f64,
    view: BookView,
    intents: Vec<Intent>,
    seq: u64,
    pub book: OrderBook,
    buf: EventBuffer,
    pub stats: Stats,
    /// Most recent fills (price, qty), newest last. Bounded; for display.
    pub recent_trades: Vec<(i64, u64)>,
}

impl Sim {
    pub fn new(sc: SimConfig) -> Self {
        let mut rng = Pcg32::new(sc.seed);
        let cfg = sc.book;

        // Agents in a fixed, deterministic order. TraderId = position.
        let mut agents: Vec<Box<dyn Agent>> = Vec::new();
        for _ in 0..sc.market_makers {
            let i = agents.len() as u32;
            agents.push(Box::new(MarketMaker::new(i, cfg, &mut rng)));
        }
        for _ in 0..sc.momentum {
            let i = agents.len() as u32;
            agents.push(Box::new(Momentum::new(i, &mut rng)));
        }
        for _ in 0..sc.noise {
            let i = agents.len() as u32;
            agents.push(Box::new(Noise::new(i, cfg, &mut rng)));
        }
        for _ in 0..sc.adversarial {
            let i = agents.len() as u32;
            agents.push(Box::new(Adversarial::new(i, cfg, &mut rng)));
        }

        // Reference price: GBM around the middle of the grid.
        let span = (cfg.max_price().0 - cfg.min_price) as f64;
        let ref_f = cfg.min_price as f64 + span / 2.0;

        Sim {
            sc,
            rng,
            agents,
            ref_f,
            view: BookView {
                best_bid: None,
                best_ask: None,
                ref_price: Price(ref_f as i64),
                last_trade: None,
                live_orders: 0,
            },
            intents: Vec::new(),
            seq: 0,
            book: OrderBook::new(cfg),
            buf: EventBuffer::for_book(&cfg),
            stats: Stats::default(),
            recent_trades: Vec::new(),
        }
    }

    pub fn view(&self) -> &BookView {
        &self.view
    }

    /// One simulation tick. Input events generated this tick are appended
    /// to `log_sink` (pass None to discard). Returns events applied.
    pub fn tick(&mut self, mut log_sink: Option<&mut Vec<InputEvent>>) -> u64 {
        let cfg = self.sc.book;
        // Advance the true-value process (floats live HERE and only here).
        self.ref_f *= (self.sc.sigma * self.rng.gauss()).exp();
        self.ref_f = self
            .ref_f
            .clamp(cfg.min_price as f64 + 1.0, cfg.max_price().0 as f64 - 1.0);
        self.view.ref_price = Price(self.ref_f as i64);

        let mut applied = 0u64;
        for (a_idx, agent) in self.agents.iter_mut().enumerate() {
            self.intents.clear();
            agent.act(&self.view, &mut self.rng, &mut self.intents);
            for intent in self.intents.drain(..) {
                self.seq += 1;
                let ev = intent.into_event(self.seq, TraderId(a_idx as u32));
                if let Some(sink) = log_sink.as_deref_mut() {
                    sink.push(ev);
                }
                applied += 1;
                self.stats.inputs += 1;
                match ev {
                    InputEvent::New { .. } => self.stats.news += 1,
                    InputEvent::Cancel { .. } => self.stats.cancels += 1,
                    InputEvent::Modify { .. } => self.stats.modifies += 1,
                }

                self.buf.clear();
                self.book.apply(ev, &mut self.buf);
                for out in self.buf.as_slice() {
                    match *out {
                        OutputEvent::Ack { .. } => self.stats.acks += 1,
                        OutputEvent::Fill { price, qty, .. } => {
                            self.stats.fills += 1;
                            self.stats.traded_qty += qty.0 as u128;
                            self.view.last_trade = Some(price);
                            self.recent_trades.push((price.0, qty.0));
                            if self.recent_trades.len() > 64 {
                                self.recent_trades.remove(0);
                            }
                        }
                        OutputEvent::Cancelled { .. } => self.stats.cancelled_events += 1,
                        OutputEvent::Rejected { .. } => self.stats.rejects += 1,
                    }
                }
                self.view.best_bid = self.book.best_bid();
                self.view.best_ask = self.book.best_ask();
                self.view.live_orders = self.book.live_count();
            }
        }

        // Sample microstructure once per tick.
        self.stats.depth_samples += 1;
        self.stats.depth_sum += self.book.live_count() as u128;
        if let (Some(b), Some(a)) = (self.view.best_bid, self.view.best_ask) {
            self.stats.spread_samples += 1;
            self.stats.spread_sum += (a.0 - b.0) as u128;
        }
        applied
    }
}

pub fn run(sc: SimConfig) -> SimResult {
    let mut sim = Sim::new(sc);
    let mut log = Vec::with_capacity(sc.events);
    // Whole ticks only — the log may slightly exceed `events`, but it
    // stays exactly consistent with the final book state and stats
    // (truncating would break `replay(log) == final_state_hash`).
    while log.len() < sc.events {
        sim.tick(Some(&mut log));
    }
    SimResult {
        final_state_hash: sim.book.state_hash(),
        stats: sim.stats,
        log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small() -> SimConfig {
        SimConfig {
            events: 200_000,
            ..SimConfig::default()
        }
    }

    /// Exit criterion: same seed -> byte-identical log (and end state).
    #[test]
    fn same_seed_identical_log() {
        let a = run(small());
        let b = run(small());
        assert_eq!(a.log, b.log);
        assert_eq!(a.final_state_hash, b.final_state_hash);
        let c = run(SimConfig {
            seed: 43,
            ..small()
        });
        assert_ne!(a.log, c.log);
    }

    /// Exit criterion: realistic microstructure — cancel rate in the
    /// vicinity of real venues (roughly 80–98% of news), plenty of
    /// resting depth, and a sane order-to-trade ratio.
    #[test]
    fn realistic_microstructure() {
        let r = run(small());
        let cancel_rate = r.stats.cancel_rate();
        assert!(
            (0.75..=1.05).contains(&cancel_rate),
            "cancel rate {cancel_rate:.2} outside the realistic band"
        );
        assert!(
            r.stats.order_to_trade() >= 3.0,
            "order-to-trade {:.1} too low to be realistic",
            r.stats.order_to_trade()
        );
        assert!(r.stats.mean_depth() >= 20.0, "book unrealistically thin");
        assert!(r.stats.fills > 0, "no trading at all");
    }
}
