//! Seeded PCG32. The simulator's ONLY source of randomness.
//!
//! No `thread_rng`, no OS entropy, no time seeding — `--seed 42` must
//! produce a byte-identical market on every run, forever. (The engine
//! itself has no RNG at all; this crate is outside the core.)

#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    pub fn new(seed: u64) -> Self {
        let mut rng = Pcg32 {
            state: 0,
            inc: (seed << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    pub fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | self.next_u32() as u64
    }

    /// Uniform in [0, n). n must be > 0.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// Uniform in [lo, hi).
    pub fn range_i64(&mut self, lo: i64, hi: i64) -> i64 {
        lo + self.below((hi - lo) as u64) as i64
    }

    /// True with probability pct/100.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }

    /// Uniform float in [0, 1). Floats are permitted HERE (sim price
    /// process and stats) and nowhere near the engine.
    pub fn unit_f64(&mut self) -> f64 {
        (self.next_u32() as f64) / (u32::MAX as f64 + 1.0)
    }

    /// Approximately standard-normal (Irwin–Hall, 12 uniforms).
    pub fn gauss(&mut self) -> f64 {
        (0..12).map(|_| self.unit_f64()).sum::<f64>() - 6.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_stream() {
        let a: Vec<u32> = {
            let mut r = Pcg32::new(42);
            (0..64).map(|_| r.next_u32()).collect()
        };
        let b: Vec<u32> = {
            let mut r = Pcg32::new(42);
            (0..64).map(|_| r.next_u32()).collect()
        };
        assert_eq!(a, b);
        let c: Vec<u32> = {
            let mut r = Pcg32::new(43);
            (0..64).map(|_| r.next_u32()).collect()
        };
        assert_ne!(a, c);
    }
}
