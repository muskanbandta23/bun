//! What a shuffle asks for, and what instructions that move lanes give.

use core::array;

pub(crate) use super::lanes::ZERO;
use super::lanes::{count, splat};
use crate::operand::Size;

/// A lane that nothing reads.
pub(crate) const ANY: u8 = 0xff;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    First,
    Second,
}

impl Side {
    pub(crate) const BOTH: [Self; 2] = [Self::First, Self::Second];

    #[inline]
    pub(crate) fn pick<T>(self, first: T, second: T) -> T {
        match self {
            Self::First => first,
            Self::Second => second,
        }
    }

    #[inline]
    pub(crate) fn other(self) -> Self {
        self.pick(Self::Second, Self::First)
    }
}

// A set of bytes is 16 bits, or a vector of flags: bytes that are 0 or 1.

const PLACES: u128 = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;

const fn every(byte: u8) -> u128 {
    byte as u128 * const { splat(Size::S8, 1) }
}

const fn bit_flags(vector: u128, bit: u8) -> u128 {
    vector >> bit & every(1)
}

/// For bytes up to 128.
const fn nonzero_flags(vector: u128) -> u128 {
    bit_flags(vector + every(0x7f), 7)
}

const fn filled(flags: u128) -> u128 {
    (flags << 8).wrapping_sub(flags)
}

fn set_of(flags: u128) -> u16 {
    let gather = |half: u128| ((half as u64).wrapping_mul(0x0102_0408_1020_4080) >> 56) as u8;
    u16::from_le_bytes([gather(flags), gather(flags >> 64)])
}

fn flags_of(set: u16) -> u128 {
    let [low, high] = set.to_le_bytes().map(|half| every(half) >> 64);
    nonzero_flags((low | high << 64) & const { splat(Size::S64, 0x8040_2010_0804_0201) })
}

/// All ones in the bytes of the set.
pub(crate) fn mask_of(bytes: u16) -> u128 {
    filled(flags_of(bytes))
}

/// The set of lanes that have any of the bytes.
pub(crate) fn lanes_with(size: Size, bytes: u16) -> u16 {
    let lane = (1 << size.bytes()) - 1;
    (0..count(size)).fold(0, |set, index| {
        set | u16::from(bytes >> (index * size.bytes()) & lane != 0) << index
    })
}

/// All bytes of the lanes that have any of the bytes.
pub(crate) fn whole_lanes(size: Size, bytes: u16) -> u16 {
    let steps = [(0x5555, 1), (0x3333, 2), (0x0f0f, 4)];
    let steps = &steps[..size.log2_bytes().into()];
    steps.iter().fold(bytes, |set, (lower, distance)| {
        set | set >> distance & lower | (set & lower) << distance
    })
}

/// For each byte of a result an index into the 32 bytes of two vectors, [`ZERO`] or [`ANY`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Pattern([u8; 16]);

impl Pattern {
    pub(crate) const IDENTITY: Self = Self::from_bits(PLACES);

    pub(crate) fn new(lanes: [u8; 16]) -> Self {
        Self(lanes.map(|index| if index < 32 { index } else { ZERO }))
    }

    const fn from_bits(bits: u128) -> Self {
        Self(bits.to_le_bytes())
    }

    const fn bits(self) -> u128 {
        u128::from_le_bytes(self.0)
    }

    /// From indices into the lanes of both vectors.
    pub(crate) fn of_lanes<const N: usize>(sources: [u8; N]) -> Self {
        let bytes = 16 / N;
        Self(array::from_fn(|byte| {
            sources[byte / bytes] * bytes as u8 + (byte % bytes) as u8
        }))
    }

    /// `first_bytes` has in all bytes of a lane the index for the first of them.
    const fn of_first_bytes(size: Size, first_bytes: u128) -> Self {
        Self::from_bits(first_bytes | PLACES & every(size.bytes() - 1))
    }

    /// The 16 bytes from `first` on.
    pub(crate) const fn window(first: u8) -> Self {
        Self::from_bits(PLACES + every(first))
    }

    pub(crate) const fn duplicate(size: Size, lane: u8) -> Self {
        Self::of_first_bytes(size, every(lane * size.bytes()))
    }

    /// Alternates between the low or the high halves.
    pub(crate) const fn zip(size: Size, high: bool) -> Self {
        let side = bit_flags(PLACES, size.log2_bytes()) << 4;
        let in_half = PLACES >> 1 & every(8 - size.bytes());
        Self::of_first_bytes(size, side | every(high as u8 * 8) | in_half)
    }

    /// The low or the high half, with lanes of `size` extended to twice that.
    pub(crate) fn zero_extend(size: Size, high: bool) -> Self {
        Self::zip(size, high).replace(Side::Second, ZERO)
    }

    /// The bytes in the set are from the second vector.
    pub(crate) fn blend(second: u16) -> Self {
        Self::from_bits(PLACES | flags_of(second) << 4)
    }

    /// Moves the bytes of each group of `group` bytes down, or up if `by` is negative, and fills in zeros.
    pub(crate) fn shift(group: u8, by: i8) -> Self {
        const BIAS: u8 = 64;
        let moved = |places: u128| (places + every(BIAS.wrapping_add_signed(by))) ^ every(BIAS);
        let outside = nonzero_flags(moved(PLACES & every(group - 1)) & !every(group - 1));
        Self::from_bits(moved(PLACES)).put(outside, ZERO)
    }

    fn put(self, flags: u128, byte: u8) -> Self {
        Self::from_bits(self.bits() & !filled(flags) | every(byte) & filled(flags))
    }

    fn index_flags(self) -> u128 {
        bit_flags(!self.bits(), 7)
    }

    fn any_flags(self) -> u128 {
        bit_flags(self.bits(), 6)
    }

    fn side_flags(self, side: Side) -> u128 {
        self.index_flags() & bit_flags(side.pick(!self.bits(), self.bits()), 4)
    }

    /// For the vectors in the other order.
    pub(crate) fn swapped(self) -> Self {
        Self::from_bits(self.bits() ^ self.index_flags() << 4)
    }

    /// For when both vectors are the same.
    pub(crate) fn folded(self) -> Self {
        Self::from_bits(self.bits() & !(self.index_flags() << 4))
    }

    /// With `by` for what comes from `side`.
    pub(crate) fn replace(self, side: Side, by: u8) -> Self {
        self.put(self.side_flags(side), by)
    }

    /// With `by` in the bytes of the set.
    pub(crate) fn with(self, bytes: u16, by: u8) -> Self {
        self.put(flags_of(bytes), by)
    }

    /// The bytes of the result that come from `side`.
    pub(crate) fn taken_from(self, side: Side) -> u16 {
        set_of(self.side_flags(side))
    }

    /// The bytes of each side that are read.
    pub(crate) fn read(self) -> [u16; 2] {
        let indices = self.0.into_iter().map(u32::from);
        let both = indices.fold(0, |set, index| set | 1u32.unbounded_shl(index));
        [both as u16, (both >> 16) as u16]
    }

    pub(crate) fn has_zero(self) -> bool {
        self.index_flags() | self.any_flags() != every(1)
    }

    /// The side that is all that is read.
    pub(crate) fn only_side(self) -> Option<Side> {
        Side::BOTH
            .into_iter()
            .find(|side| self.side_flags(side.other()) == 0)
    }

    pub(crate) fn accepts(self, result: Self) -> bool {
        (self.bits() ^ result.bits()) & !filled(self.any_flags()) == 0
    }

    /// Every byte that is read stays in its place among `group` bytes.
    pub(crate) fn keeps_places(self, group: u8) -> bool {
        (self.bits() ^ PLACES) & every(group - 1) & filled(self.index_flags()) == 0
    }

    /// The place of the first byte that is not zero, and its index.
    pub(crate) fn first_read(self) -> Option<(u8, u8)> {
        let place = self.index_flags().trailing_zeros() as usize / 8;
        Some((place as u8, *self.0.get(place)?))
    }

    /// The same in terms of `N` lanes.
    pub(crate) fn widen<const N: usize>(self) -> Option<[u8; N]> {
        let bytes = 16 / N;
        if !self.keeps_places(bytes as u8) {
            return None;
        }
        let mut wide = [ANY; N];
        for (byte, index) in self.0.into_iter().enumerate() {
            let lane = if index < 32 {
                index / bytes as u8
            } else {
                index
            };
            let known = &mut wide[byte / bytes];
            if *known == ANY {
                *known = lane;
            } else if lane != *known && lane != ANY {
                return None;
            }
        }
        Some(wide)
    }

    /// Reads from the one vector that `before` gives.
    pub(crate) fn of(self, before: Self) -> Self {
        let through = |index: u8| *before.0.get(usize::from(index)).unwrap_or(&index);
        Self(self.0.map(through))
    }

    /// What is left to do with the one vector that `step` gives, which has all that is read and no zeros.
    pub(crate) fn after(self, step: Self) -> Self {
        let mut place = [ANY; 32];
        for (byte, index) in (0..).zip(step.0) {
            place[usize::from(index)] = byte;
        }
        let rest = Self(
            self.0
                .map(|index| *place.get(usize::from(index)).unwrap_or(&index)),
        );
        debug_assert!(rest.any_flags() == self.any_flags());
        rest
    }

    /// For a table lookup that gives zero for indices from 128 up.
    pub(crate) fn indices(self) -> u128 {
        self.put(self.any_flags(), ZERO).bits()
    }
}
