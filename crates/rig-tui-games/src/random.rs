//! A tiny reproducible game RNG; never used for identity or authorization.

pub(super) struct Random(u64);

impl Random {
    pub(super) fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9e3779b97f4a7c15 } else { seed })
    }

    pub(super) fn index(&mut self, upper: usize) -> usize {
        assert!(upper > 0, "game random choice needs an item");
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % upper as u64) as usize
    }
}
