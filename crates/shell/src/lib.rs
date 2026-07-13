//! The imperative shell: everything the engine core is forbidden to do.
//!
//! The shell reads the clock, assigns sequence numbers, touches files and
//! the terminal, and runs threads. The engine does none of these — that
//! separation is what makes determinism (and therefore replay and the
//! differential fuzzer) possible.

pub mod codec;
pub mod journal;
pub mod ring;
pub mod tui;
