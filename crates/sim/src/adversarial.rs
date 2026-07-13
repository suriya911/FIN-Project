//! Adversarial agent: tries to break the engine, not to make money.
//! Hammers a single level, runs submit-cancel tight loops, parks orders
//! at the grid edges, and fires exact-fill and FOK probes at the touch.

use crate::agent::{Agent, BookView, IdGen, Intent};
use crate::rng::Pcg32;
use tessera_core::{BookConfig, OrderId, Price, Qty, SelfTradePrevention, Side, TimeInForce};

pub struct Adversarial {
    ids: IdGen,
    cfg: BookConfig,
    /// Ids from the submit-cancel loop awaiting their cancel.
    pending_cancel: Vec<OrderId>,
    /// The one level this agent hammers.
    target_level: i64,
}

impl Adversarial {
    pub fn new(agent_index: u32, cfg: BookConfig, rng: &mut Pcg32) -> Self {
        let span = cfg.max_price().0 - cfg.min_price;
        Adversarial {
            ids: IdGen::for_agent(agent_index),
            cfg,
            pending_cancel: Vec::new(),
            target_level: cfg.min_price + span / 2 + rng.range_i64(-4, 5),
        }
    }
}

impl Agent for Adversarial {
    fn act(&mut self, view: &BookView, rng: &mut Pcg32, out: &mut Vec<Intent>) {
        // Flush yesterday's submit-cancel loop first (tight churn).
        for id in self.pending_cancel.drain(..) {
            out.push(Intent::Cancel { order_id: id });
        }

        match rng.below(4) {
            // Hammer one price level with a burst.
            0 => {
                let n = 1 + rng.below(8);
                for _ in 0..n {
                    let id = self.ids.next();
                    out.push(Intent::New {
                        order_id: id,
                        side: if rng.chance(50) { Side::Bid } else { Side::Ask },
                        price: Price(self.target_level),
                        qty: Qty(1 + rng.below(10)),
                        tif: TimeInForce::Gtc,
                        stp: SelfTradePrevention::CancelBoth,
                    });
                    if rng.chance(70) {
                        self.pending_cancel.push(id);
                    }
                }
            }
            // Orders at the exact book edges.
            1 => {
                let (price, side) = if rng.chance(50) {
                    (Price(self.cfg.min_price), Side::Bid)
                } else {
                    (self.cfg.max_price(), Side::Ask)
                };
                let id = self.ids.next();
                self.pending_cancel.push(id);
                out.push(Intent::New {
                    order_id: id,
                    side,
                    price,
                    qty: Qty(1),
                    tif: TimeInForce::Gtc,
                    stp: SelfTradePrevention::None,
                });
            }
            // Exact-fill probe: IOC for precisely what's quoted at the touch.
            2 => {
                if let Some(ask) = view.best_ask {
                    out.push(Intent::New {
                        order_id: self.ids.next(),
                        side: Side::Bid,
                        price: ask,
                        qty: Qty(1 + rng.below(30)),
                        tif: TimeInForce::Ioc,
                        stp: SelfTradePrevention::CancelResting,
                    });
                }
            }
            // FOK probes that mostly can't fill (pre-check stress).
            _ => {
                if let Some(bid) = view.best_bid {
                    out.push(Intent::New {
                        order_id: self.ids.next(),
                        side: Side::Ask,
                        price: bid,
                        qty: Qty(500 + rng.below(5_000)),
                        tif: TimeInForce::Fok,
                        stp: SelfTradePrevention::None,
                    });
                }
            }
        }
    }
}
