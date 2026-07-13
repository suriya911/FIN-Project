//! Noise trader: Poisson-arrival random orders around the mid. Baseline
//! load, resting liquidity at depth, and occasional stale-order cancels.

use crate::agent::{Agent, BookView, IdGen, Intent};
use crate::rng::Pcg32;
use tessera_core::{BookConfig, OrderId, Price, Qty, SelfTradePrevention, Side, TimeInForce};

pub struct Noise {
    ids: IdGen,
    cfg: BookConfig,
    /// Arrival probability per tick, in percent.
    arrival_pct: u64,
    resting: Vec<OrderId>,
}

impl Noise {
    pub fn new(agent_index: u32, cfg: BookConfig, rng: &mut Pcg32) -> Self {
        Noise {
            ids: IdGen::for_agent(agent_index),
            cfg,
            arrival_pct: 10 + rng.below(20),
            resting: Vec::new(),
        }
    }
}

impl Agent for Noise {
    fn act(&mut self, view: &BookView, rng: &mut Pcg32, out: &mut Vec<Intent>) {
        // Occasionally clean up an old resting order.
        if !self.resting.is_empty() && rng.chance(15) {
            let idx = rng.below(self.resting.len() as u64) as usize;
            out.push(Intent::Cancel {
                order_id: self.resting.swap_remove(idx),
            });
        }

        if !rng.chance(self.arrival_pct) {
            return;
        }

        let center = view.mid().unwrap_or(view.ref_price).0;
        // Geometric-ish offset: mostly at/near the touch, occasionally deep.
        let mut off = 0i64;
        while rng.chance(60) && off < 20 {
            off += 1;
        }
        let side = if rng.chance(50) { Side::Bid } else { Side::Ask };
        let raw = match side {
            Side::Bid => center - off,
            Side::Ask => center + off,
        };
        let price = Price(raw.clamp(self.cfg.min_price, self.cfg.max_price().0));
        let tif = if rng.chance(25) {
            TimeInForce::Ioc
        } else {
            TimeInForce::Gtc
        };
        let id = self.ids.next_id();
        if tif == TimeInForce::Gtc {
            self.resting.push(id);
        }
        out.push(Intent::New {
            order_id: id,
            side,
            price,
            qty: Qty(1 + rng.below(50)),
            tif,
            stp: SelfTradePrevention::None,
        });
    }
}
