//! Tessera engine core: a deterministic, replayable limit order book.
//!
//! This crate is a pure state machine. No clocks, no RNG, no syscalls,
//! no threads, no floats, and no allocation in the hot path.

#![no_std]

extern crate alloc;
