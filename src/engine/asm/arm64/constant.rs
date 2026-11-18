//! The shortest way to put a constant in a register.

use super::assembler::Assembler;
use super::immediate::BitmaskImm;
use super::opcode::{Logical, MoveWide};
use super::registers::ZR;
use crate::operand::Width;
use crate::register::Gpr;

const PART_BITS: u32 = 16;
const PART: u64 = 0xffff;
const IN_EVERY_PART: u64 = 0x0001_0001_0001_0001;
const LOW_HALF: u64 = 0xffff_ffff;

#[derive(Clone, Copy)]
enum First {
    Wide(MoveWide, Width, u16, u8),
    Bitmask(BitmaskImm),
}

/// One instruction that sets all bits, and a `movk` for each 16 that it gets wrong.
#[derive(Clone, Copy)]
pub struct ConstantMove {
    first: First,
    value: u64,
    /// The top bit of each part that is wrong.
    wrong: u64,
}

/// The top bit of each part that is not zero.
#[inline]
fn nonzero_parts(value: u64) -> u64 {
    const LOW_BITS: u64 = 0x7fff * IN_EVERY_PART;
    (((value & LOW_BITS) + LOW_BITS) | value) & !LOW_BITS
}

/// For the top bits of parts.
#[inline]
fn count_parts(parts: u64) -> u32 {
    ((parts >> (PART_BITS - 1)).wrapping_mul(IN_EVERY_PART) >> (3 * PART_BITS)) as u32
}

fn repeated_part(value: u64, index: u32) -> u64 {
    (value >> (index * PART_BITS) & PART) * IN_EVERY_PART
}

fn repeated_half(half: u64) -> u64 {
    half << 32 | half & LOW_HALF
}

impl ConstantMove {
    /// Of 32 bits, the low half of `value` counts.
    pub fn new(width: Width, value: u64) -> Self {
        if width == Width::W32 || value <= LOW_HALF {
            return Self::within_low_half(value & LOW_HALF);
        }
        let (set, clear) = (nonzero_parts(value), nonzero_parts(!value));
        let (set_count, clear_count) = (count_parts(set), count_parts(clear));
        let wide = if set_count <= clear_count {
            Self::wide(MoveWide::Movz, Width::W64, value, value, set)
        } else {
            Self::wide(MoveWide::Movn, Width::W64, value, !value, clear)
        };
        let count = set_count.min(clear_count);
        if count <= 1 {
            return wide;
        }
        Self::bitmask(Width::W64, value, value)
            .or_else(|| Self::mixed(value, count))
            .unwrap_or(wide)
    }

    #[inline]
    pub fn instructions(self) -> u32 {
        1 + count_parts(self.wrong)
    }

    /// The instruction takes the lowest of `parts` of `bits`.
    #[inline]
    fn wide(op: MoveWide, width: Width, value: u64, bits: u64, parts: u64) -> Self {
        let shift = parts.trailing_zeros() % 64 / PART_BITS * PART_BITS;
        Self {
            first: First::Wide(op, width, (bits >> shift) as u16, shift as u8),
            value,
            wrong: parts & parts.wrapping_sub(1),
        }
    }

    #[inline]
    fn bitmask(width: Width, value: u64, pattern: u64) -> Option<Self> {
        Some(Self {
            first: First::Bitmask(BitmaskImm::new(width, pattern)?),
            value,
            wrong: nonzero_parts(value ^ pattern),
        })
    }

    fn within_low_half(value: u64) -> Self {
        let inverted = value ^ LOW_HALF;
        let (set, clear) = (nonzero_parts(value), nonzero_parts(inverted));
        let zeros = Self::wide(MoveWide::Movz, Width::W32, value, value, set);
        if zeros.wrong == 0 {
            return zeros;
        }
        let ones = Self::wide(MoveWide::Movn, Width::W32, value, inverted, clear);
        if ones.wrong == 0 {
            return ones;
        }
        Self::bitmask(Width::W32, value, value).unwrap_or(zeros)
    }

    /// A first instruction of 32 bits or with a bitmask, if that makes fewer than `count`.
    fn mixed(value: u64, count: u32) -> Option<Self> {
        if count == 2 {
            return None;
        }
        let upper = nonzero_parts(value & !LOW_HALF);
        if upper.is_power_of_two() {
            let low = Self::within_low_half(value & LOW_HALF);
            if low.wrong == 0 {
                return Some(Self {
                    value,
                    wrong: upper,
                    ..low
                });
            }
        }
        // Each part of a bitmask is one when repeated, or has the same bit throughout.
        let could_be_right = (0..4)
            .map(|index| repeated_part(value, index))
            .filter(|&part| {
                part == 0 || part == u64::MAX || BitmaskImm::new(Width::W64, part).is_some()
            })
            .count() as u32;
        if 5 - could_be_right >= count {
            return None;
        }
        let with_one_part_changed = (0..4).flat_map(|index| {
            let part = PART << (index * PART_BITS);
            [0, u64::MAX, value.rotate_left(32)].map(|other| value & !part | other & part)
        });
        let (even, odd) = (0x0000_ffff_0000_ffff, 0xffff_0000_ffff_0000);
        let with_two_parts_kept = (0..3).map(|index| repeated_part(value, index)).chain([
            repeated_half(value),
            repeated_half(value >> 32),
            repeated_half(value & PART | value >> 32 & !PART),
            repeated_half(value >> 32 & PART | value & !PART),
            value & even | odd << 32,
            value & even | odd >> 32,
            value & odd | even << 32,
            value & odd | even >> 32,
        ]);
        with_one_part_changed
            .chain(with_two_parts_kept)
            .filter_map(|pattern| Self::bitmask(Width::W64, value, pattern))
            .find(|found| found.instructions() < count)
    }
}

impl Assembler {
    /// One to four instructions that leave the flags alone.
    pub fn move_constant(&mut self, dst: Gpr, constant: ConstantMove) {
        match constant.first {
            First::Wide(op, width, imm, shift) => self.move_wide(op, width, dst, imm, shift),
            First::Bitmask(imm) => self.logical_imm(Logical::Orr, dst, ZR, imm),
        }
        let width = if constant.value <= LOW_HALF {
            Width::W32
        } else {
            Width::W64
        };
        let mut wrong = constant.wrong;
        while wrong != 0 {
            let shift = wrong.trailing_zeros() / PART_BITS * PART_BITS;
            let part = (constant.value >> shift) as u16;
            self.move_wide(MoveWide::Movk, width, dst, part, shift as u8);
            wrong &= wrong - 1;
        }
    }
}
