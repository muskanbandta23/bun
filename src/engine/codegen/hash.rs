use core::hash::Hasher;

/// For keys made of a few small integers.
#[derive(Default, Clone, Copy)]
pub(crate) struct FastHasher(u64);

impl FastHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0 ^ word)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .rotate_left(26);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<4>();
        words
            .iter()
            .for_each(|&word| self.add(u32::from_le_bytes(word).into()));
        rest.iter().for_each(|&byte| self.add(byte.into()));
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(value.into());
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(value.into());
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
}
