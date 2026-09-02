//! Tiny deterministic PRNG (xorshift64*) that is part of the rollback state.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Avoid the all-zero state; mix the seed a little.
        let s = seed ^ 0x9E37_79B9_7F4A_7C15;
        Rng { state: if s == 0 { 0x2545_F491_4F6C_DD1D } else { s } }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Uniform float in `[lo, hi)`.
    pub fn random_float(&mut self, lo: f32, hi: f32) -> f32 {
        let unit = (self.next_u32() >> 8) as f32 / 16_777_216.0;
        lo + (hi - lo) * unit
    }

    /// Uniform integer in `[lo, hi]`.
    pub fn random_int(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let range = (hi - lo + 1) as u32;
        lo + (self.next_u32() % range) as i32
    }
}
