//! The vector instructions that operations on integers and floats use.

use super::assembler::{Assembler, fpr};
use crate::family::family;
use crate::register::Fpr;

family! {
    /// `Bsl` selects by the bits of `dst`. `Bit` and `Bif` insert `lhs` where the bits of `rhs` are set or clear.
    BasicBitwise: u32 {
        Orr = 2 << 22,
        Eor = 1 << 29,
        Bsl = 1 << 29 | 1 << 22,
        Bit = 1 << 29 | 2 << 22,
        Bif = 1 << 29 | 3 << 22,
    }
}

impl Assembler {
    /// On all 16 bytes.
    #[inline]
    pub fn basic_bitwise(&mut self, op: BasicBitwise, dst: Fpr, lhs: Fpr, rhs: Fpr) {
        self.emit(0x4e20_1c00 | op as u32 | fpr(rhs) << 16 | fpr(lhs) << 5 | fpr(dst));
    }

    /// `movi dst.2d, #0`.
    #[inline]
    pub fn basic_zero(&mut self, dst: Fpr) {
        self.emit(0x6f00_e400 | fpr(dst));
    }

    /// `movi dst.2s, #0x80, lsl #24`.
    pub fn basic_sign_bits32(&mut self, dst: Fpr) {
        self.emit(0x0f04_6400 | fpr(dst));
    }

    /// `cnt dst.8b, src.8b`.
    pub fn basic_count_ones(&mut self, dst: Fpr, src: Fpr) {
        self.emit(0x0e20_5800 | fpr(src) << 5 | fpr(dst));
    }

    /// `addv` of the low 8 bytes.
    pub fn basic_add_bytes(&mut self, dst: Fpr, src: Fpr) {
        self.emit(0x0e31_b800 | fpr(src) << 5 | fpr(dst));
    }
}
