//! Platform-independent state checksum for desync detection.
//!
//! `std`'s `DefaultHasher` (and SeaHash, which bevy_ggrs uses) are stable across runs, but the
//! `Hash` trait feeds them `usize` / `isize` values in the platform's native width: slice and
//! `Vec` lengths (`write_length_prefix`) and `#[derive(Hash)]` enum discriminants (`Option`,
//! `Phase`, `HitEnt`). The same simulation state therefore hashes to different values on wasm32
//! (4-byte lengths) and x86_64 / aarch64 (8-byte lengths), and a desktop client playing a browser
//! client reports a desync at every check even though both simulate identically.
//!
//! This hasher widens every integer to a fixed width and mixes the bytes with a fixed algorithm
//! (FNV-1a with a MurmurHash3 finaliser), so every platform produces the same checksum for the
//! same state.

use std::hash::Hasher;

#[derive(Clone, Copy, Debug)]
pub struct DetHasher(u64);

impl Default for DetHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl DetHasher {
    pub const fn new() -> Self {
        DetHasher(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for DetHasher {
    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^= x >> 33;
        x
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    // Fixed widths and byte order regardless of the platform.
    fn write_u8(&mut self, n: u8) {
        self.write(&n.to_le_bytes());
    }
    fn write_u16(&mut self, n: u16) {
        self.write(&n.to_le_bytes());
    }
    fn write_u32(&mut self, n: u32) {
        self.write(&n.to_le_bytes());
    }
    fn write_u64(&mut self, n: u64) {
        self.write(&n.to_le_bytes());
    }
    fn write_u128(&mut self, n: u128) {
        self.write(&n.to_le_bytes());
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }
    fn write_i8(&mut self, n: i8) {
        self.write_u8(n as u8);
    }
    fn write_i16(&mut self, n: i16) {
        self.write_u16(n as u16);
    }
    fn write_i32(&mut self, n: i32) {
        self.write_u32(n as u32);
    }
    fn write_i64(&mut self, n: i64) {
        self.write_u64(n as u64);
    }
    fn write_i128(&mut self, n: i128) {
        self.write_u128(n as u128);
    }
    fn write_isize(&mut self, n: isize) {
        self.write_i64(n as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    #[test]
    fn lengths_and_discriminants_are_fixed_width() {
        // The values a `Vec` / `Option` feed through `write_usize` / `write_isize` must land in
        // the stream as eight little-endian bytes whatever the pointer width.
        let mut h = DetHasher::new();
        vec![1u32, 2, 3].hash(&mut h);
        Some(7u8).hash(&mut h);
        Option::<u8>::None.hash(&mut h);

        let mut expected = DetHasher::new();
        expected.write_u64(3); // length prefix
        expected.write_u32(1);
        expected.write_u32(2);
        expected.write_u32(3);
        expected.write_u64(1); // Some
        expected.write_u8(7);
        expected.write_u64(0); // None
        assert_eq!(h.finish(), expected.finish());
    }
}
