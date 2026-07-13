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

pub mod config;
pub mod events;
pub mod types;

pub use config::BookConfig;
pub use events::{CancelReason, InputEvent, OutputEvent, RejectReason};
pub use types::{OrderId, Price, Qty, SelfTradePrevention, Seq, Side, TimeInForce, TraderId};
