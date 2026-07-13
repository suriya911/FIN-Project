//! Momentum trader: chases directional flow by crossing the spread.
//! Stresses the match loop and multi-level sweeps.

use crate::agent::{Agent, BookView, IdGen, Intent};
use crate::rng::Pcg32;
use tessera_core::{Price, Qty, SelfTradePrevention, Side, TimeInForce};

pub struct Momentum {
    ids: IdGen,
    last_seen: Option<Price>,
    /// Fires on roughly this % of ticks.
    activity_pct: u64,
}

impl Momentum {
    pub fn new(agent_index: u32, rng: &mut Pcg32) -> Self {
        Momentum {
            ids: IdGen::for_agent(agent_index),
            last_seen: None,
            activity_pct: 3 + rng.below(7),
        }
    }
}

impl Agent for Momentum {
    fn act(&mut self, view: &BookView, rng: &mut Pcg32, out: &mut Vec<Intent>) {
        let Some(trade) = view.last_trade else { return };
        let prev = self.last_seen.replace(trade);
        let Some(prev) = prev else { return };
        if trade == prev || !rng.chance(self.activity_pct) {
            return;
        }

        // Price moved: chase it with a marketable IOC. Sweeping several
        // levels is the point.
        let up = trade > prev;
        let (side, limit) = if up {
            match view.best_ask {
                Some(a) => (Side::Bid, Price(a.0 + rng.range_i64(0, 4))),
                None => return,
            }
        } else {
            match view.best_bid {
                Some(b) => (Side::Ask, Price(b.0 - rng.range_i64(0, 4))),
                None => return,
            }
        };
        out.push(Intent::New {
            order_id: self.ids.next_id(),
            side,
            price: limit,
            qty: Qty(10 + rng.below(190)),
            tif: TimeInForce::Ioc,
            stp: SelfTradePrevention::None,
        });
    }
}
