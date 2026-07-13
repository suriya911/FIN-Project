//! Market maker: quotes both sides around the reference price and
//! requotes whenever it drifts. This agent is what produces the realistic
//! ~90% cancel rate — almost every quote it posts dies by cancel, not by
//! fill, exactly like production market-making flow.

use crate::agent::{Agent, BookView, IdGen, Intent};
use crate::rng::Pcg32;
use tessera_core::{BookConfig, OrderId, Price, Qty, SelfTradePrevention, Side, TimeInForce};

pub struct MarketMaker {
    ids: IdGen,
    cfg: BookConfig,
    half_spread: i64,
    quote_size: u64,
    /// Requote when |quote center - ref| exceeds this many ticks.
    tolerance: i64,
    live_bid: Option<(OrderId, Price)>,
    live_ask: Option<(OrderId, Price)>,
}

impl MarketMaker {
    pub fn new(agent_index: u32, cfg: BookConfig, rng: &mut Pcg32) -> Self {
        MarketMaker {
            ids: IdGen::for_agent(agent_index),
            cfg,
            half_spread: 1 + rng.below(3) as i64,
            quote_size: 5 + rng.below(45),
            tolerance: rng.below(2) as i64, // 0 = twitchy, 1 = calmer
            live_bid: None,
            live_ask: None,
        }
    }

    fn clamp(&self, p: i64) -> Price {
        Price(p.clamp(self.cfg.min_price, self.cfg.max_price().0))
    }
}

impl Agent for MarketMaker {
    fn act(&mut self, view: &BookView, rng: &mut Pcg32, out: &mut Vec<Intent>) {
        let r = view.ref_price.0;
        let want_bid = self.clamp(r - self.half_spread);
        let want_ask = self.clamp(r + self.half_spread);
        if want_bid >= want_ask {
            return; // ref pinned against a grid edge; sit out this tick
        }

        let drifted = |cur: Option<(OrderId, Price)>, want: Price| match cur {
            None => true,
            Some((_, p)) => (p.0 - want.0).abs() > self.tolerance,
        };

        if drifted(self.live_bid, want_bid) || drifted(self.live_ask, want_ask) {
            // Cancel-and-replace both sides. THIS is the cancel storm.
            if let Some((id, _)) = self.live_bid.take() {
                out.push(Intent::Cancel { order_id: id });
            }
            if let Some((id, _)) = self.live_ask.take() {
                out.push(Intent::Cancel { order_id: id });
            }
            let bid_id = self.ids.next();
            let ask_id = self.ids.next();
            let qty = Qty(self.quote_size + rng.below(5));
            out.push(Intent::New {
                order_id: bid_id,
                side: Side::Bid,
                price: want_bid,
                qty,
                tif: TimeInForce::Gtc,
                stp: SelfTradePrevention::CancelResting,
            });
            out.push(Intent::New {
                order_id: ask_id,
                side: Side::Ask,
                price: want_ask,
                qty,
                tif: TimeInForce::Gtc,
                stp: SelfTradePrevention::CancelResting,
            });
            self.live_bid = Some((bid_id, want_bid));
            self.live_ask = Some((ask_id, want_ask));
        }
    }
}
