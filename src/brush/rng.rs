//! PCG32 (O'Neill's pcg32_random_r): a small seeded generator for spray
//! strokes. The same seed gives the same dabs on every machine, so spray
//! strokes replay exactly.

#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    pub fn new(seed: u64, stream: u64) -> Pcg32 {
        let mut r = Pcg32 { state: 0, inc: (stream << 1) | 1 };
        r.next_u32();
        r.state = r.state.wrapping_add(seed);
        r.next_u32();
        r
    }

    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in [0, 1), from 24 random bits (exact in f32).
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in the unit disk, by rejection (no trigonometry, so it is
    /// the same on every platform).
    pub fn in_disk(&mut self) -> (f32, f32) {
        loop {
            let (x, y) = (self.next_f32() * 2.0 - 1.0, self.next_f32() * 2.0 - 1.0);
            if x * x + y * y <= 1.0 {
                return (x, y);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_reference_sequence() {
        // pcg32-global demo: seed 42, stream 54.
        let mut r = Pcg32::new(42, 54);
        let want = [0xa15c_02b7, 0x7b47_f409, 0xba1d_3330, 0x83d2_f293, 0xbfa4_784b, 0xcbed_606e];
        for w in want {
            assert_eq!(r.next_u32(), w);
        }
        let mut a = Pcg32::new(7, 0);
        let mut b = Pcg32::new(7, 0);
        assert!((0..100).all(|_| a.next_f32() == b.next_f32()));
        assert!((0..1000).all(|_| (0.0..1.0).contains(&a.next_f32())));
    }
}
