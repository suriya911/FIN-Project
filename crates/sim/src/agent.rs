//! The agent abstraction: each agent sees a cheap view of the market and
//! emits order intents. The runner sequences them, applies them to the
//! engine, and feeds the results back into the view.

use crate::rng::Pcg32;
use tessera_core::{InputEvent, OrderId, Price, Qty, TraderId};

/// What an agent is allowed to see. Deliberately shallow — real
/// participants see a market data feed, not the matching engine's guts.
#[derive(Copy, Clone, Debug)]
pub struct BookView {
    pub best_bid: Option<Price>,
    pub best_ask: Option<Price>,
    /// The exogenous reference price (the "true value" random walk), in
    /// ticks, clamped to the grid.
    pub ref_price: Price,
    pub last_trade: Option<Price>,
    /// Total resting orders (proxy for depth).
    pub live_orders: u32,
}

impl BookView {
    pub fn mid(&self) -> Option<Price> {
        match (self.best_bid, self.best_ask) {
            (Some(b), Some(a)) => Some(Price((b.0 + a.0) / 2)),
            _ => None,
        }
    }
}

/// Order intents: like InputEvent but without a Seq — sequencing is the
/// runner's job (in the real system, the shell's).
#[derive(Copy, Clone, Debug)]
pub enum Intent {
    New {
        order_id: OrderId,
        side: tessera_core::Side,
        price: Price,
        qty: Qty,
        tif: tessera_core::TimeInForce,
        stp: tessera_core::SelfTradePrevention,
    },
    Cancel {
        order_id: OrderId,
    },
    Modify {
        order_id: OrderId,
        new_price: Price,
        new_qty: Qty,
    },
}

impl Intent {
    pub fn into_event(self, seq: u64, trader: TraderId) -> InputEvent {
        match self {
            Intent::New {
                order_id,
                side,
                price,
                qty,
                tif,
                stp,
            } => InputEvent::New {
                seq: tessera_core::Seq(seq),
                order_id,
                trader,
                side,
                price,
                qty,
                tif,
                stp,
            },
            Intent::Cancel { order_id } => InputEvent::Cancel {
                seq: tessera_core::Seq(seq),
                order_id,
                trader,
            },
            Intent::Modify {
                order_id,
                new_price,
                new_qty,
            } => InputEvent::Modify {
                seq: tessera_core::Seq(seq),
                order_id,
                trader,
                new_price,
                new_qty,
            },
        }
    }
}

pub trait Agent {
    /// One simulation tick: observe, decide, emit intents.
    fn act(&mut self, view: &BookView, rng: &mut Pcg32, out: &mut Vec<Intent>);
}

/// Per-agent order id allocator: agent `a`'s ids live in a disjoint
/// namespace (top 24 bits), so agents never collide.
pub struct IdGen {
    base: u64,
    next: u64,
}

impl IdGen {
    pub fn for_agent(agent_index: u32) -> Self {
        IdGen {
            base: (agent_index as u64 + 1) << 40,
            next: 0,
        }
    }
    pub fn next(&mut self) -> OrderId {
        self.next += 1;
        OrderId(self.base | self.next)
    }
}
