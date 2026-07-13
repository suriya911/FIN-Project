//! Tessera engine core: a deterministic, replayable limit order book.
//!
//! This crate is a pure state machine:
//!
//! ```text
//! (BookState, InputEvent) -> (BookState', [OutputEvent])
//! ```
//!
//! No clocks, no RNG, no syscalls, no threads, no floats, and no
//! allocation in the hot path. `#![no_std]` makes most of that
//! mechanically impossible rather than aspirational. The only use of
//! `alloc` is pre-allocation at startup (arena, level array, buffers);
//! `apply()` never allocates.

#![no_std]

extern crate alloc;

pub mod arena;
pub mod book;
pub mod book_side;
pub mod buffer;
pub mod config;
pub mod events;
pub mod hash;
pub mod order_index;
pub mod snapshot;
pub mod types;
pub mod validate;

pub use arena::{Arena, OrderSlot, NIL};
pub use book::OrderBook;
pub use buffer::EventBuffer;
pub use config::BookConfig;
pub use events::{CancelReason, InputEvent, OutputEvent, RejectReason};
pub use types::{OrderId, Price, Qty, SelfTradePrevention, Seq, Side, TimeInForce, TraderId};
