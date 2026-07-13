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
    /// Stop once the input log reaches this many events.
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

pub fn run(sc: SimConfig) -> SimResult {
    let mut rng = Pcg32::new(sc.seed);
    let cfg = sc.book;
    let mut book = OrderBook::new(cfg);
    let mut buf = EventBuffer::for_book(&cfg);

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
    let mut ref_f = cfg.min_price as f64 + span / 2.0;

    let mut view = BookView {
        best_bid: None,
        best_ask: None,
        ref_price: Price(ref_f as i64),
        last_trade: None,
        live_orders: 0,
    };

    let mut log = Vec::with_capacity(sc.events);
    let mut stats = Stats::default();
    let mut intents: Vec<Intent> = Vec::new();
    let mut seq = 0u64;

    'outer: loop {
        // Advance the true-value process (floats live HERE and only here).
        ref_f *= (sc.sigma * rng.gauss()).exp();
        ref_f = ref_f.clamp(cfg.min_price as f64 + 1.0, cfg.max_price().0 as f64 - 1.0);
        view.ref_price = Price(ref_f as i64);

        for (a_idx, agent) in agents.iter_mut().enumerate() {
            intents.clear();
            agent.act(&view, &mut rng, &mut intents);
            for intent in intents.drain(..) {
                seq += 1;
                let ev = intent.into_event(seq, TraderId(a_idx as u32));
                log.push(ev);
                stats.inputs += 1;
                match ev {
                    InputEvent::New { .. } => stats.news += 1,
                    InputEvent::Cancel { .. } => stats.cancels += 1,
                    InputEvent::Modify { .. } => stats.modifies += 1,
                }

                buf.clear();
                book.apply(ev, &mut buf);
                for out in buf.as_slice() {
                    match *out {
                        OutputEvent::Ack { .. } => stats.acks += 1,
                        OutputEvent::Fill { price, qty, .. } => {
                            stats.fills += 1;
                            stats.traded_qty += qty.0 as u128;
                            view.last_trade = Some(price);
                        }
                        OutputEvent::Cancelled { .. } => stats.cancelled_events += 1,
                        OutputEvent::Rejected { .. } => stats.rejects += 1,
                    }
                }
                view.best_bid = book.best_bid();
                view.best_ask = book.best_ask();
                view.live_orders = book.live_count();

                if log.len() >= sc.events {
                    break 'outer;
                }
            }
        }

        // Sample microstructure once per tick.
        stats.depth_samples += 1;
        stats.depth_sum += book.live_count() as u128;
        if let (Some(b), Some(a)) = (view.best_bid, view.best_ask) {
            stats.spread_samples += 1;
            stats.spread_sum += (a.0 - b.0) as u128;
        }
    }

    SimResult {
        log,
        stats,
        final_state_hash: book.state_hash(),
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
