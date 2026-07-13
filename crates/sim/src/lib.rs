//! Agent-based market simulator.
//!
//! Benchmarking a matching engine with uniform random orders produces an
//! unrealistic book and a meaningless number. This crate generates load
//! that looks like a market: market makers that cancel-and-requote
//! constantly (~90% cancel rate), momentum flow that sweeps levels,
//! Poisson noise, and an adversarial agent that goes for the worst case.
//!
//! Everything is driven by one seeded PCG32: `--seed 42` reproduces the
//! exact same market, byte for byte, every time.

pub mod adversarial;
pub mod agent;
pub mod market_maker;
pub mod momentum;
pub mod noise;
pub mod rng;
pub mod runner;

pub use agent::{Agent, BookView, IdGen, Intent};
pub use rng::Pcg32;
pub use runner::{run, SimConfig, SimResult, Stats};
