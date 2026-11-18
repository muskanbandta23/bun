//! A double rounded to a given number of decimal digits. Ties go away from zero.

use core::iter::once;

use crate::double::decompose;
use crate::expansion::{BLOCK_DIGITS, Expansion};
use crate::powers_of_ten::{
    SMALL_POWERS, floor_log2_of_power_of_ten, floor_log10_of_power_of_two, power_of_ten,
};
use crate::sixteen_digits::write_sixteen_digits;

/// 21 digits before the point and 100 after it.
const MAX_DIGITS: usize = 121;
/// So many digits come out of one product with a power of ten.
const MAX_SCALED_DIGITS: usize = 17;

/// `0.d1d2d3... * 10^point`
pub(crate) struct Rounded {
    /// Zeros wherever nothing else is written. The first block may start with all but one of its digits unused.
    characters: [u8; MAX_DIGITS + 2 * BLOCK_DIGITS],
    start: usize,
    pub(crate) point: i32,
}

impl Rounded {
    const ZERO: Rounded = Rounded {
        characters: [b'0'; _],
        start: 0,
        point: 1,
    };

    /// The magnitude of a finite value with `count` digits, from 1 to 101.
    pub(crate) fn to_significant_digits(value: f64, count: usize) -> Rounded {
        if value == 0.0 {
            return Rounded::ZERO;
        }
        if count > MAX_SCALED_DIGITS {
            return Rounded::from_expansion(value, |_| count as i32);
        }
        let (digits, point) = scaled_digits(value, count);
        let digits = digits * SMALL_POWERS[MAX_SCALED_DIGITS - count];
        let mut rounded = Rounded {
            point,
            ..Rounded::ZERO
        };
        if let Some((sixteen, rest)) = rounded.characters.split_first_chunk_mut() {
            write_sixteen_digits(digits / 10, sixteen);
            rest[0] = b'0' + (digits % 10) as u8;
        }
        rounded
    }

    /// The magnitude of a value below 10^21 with up to 100 digits after the point.
    pub(crate) fn to_fraction_digits(value: f64, count: usize) -> Rounded {
        if value == 0.0 {
            return Rounded::ZERO;
        }
        Rounded::from_expansion(value, |point| point + count as i32)
    }

    /// `count` says how many digits to keep given how many precede the point.
    fn from_expansion(value: f64, count: impl FnOnce(i32) -> i32) -> Rounded {
        let (significand, exponent) = decompose(value);
        let mut expansion = Expansion::new(significand, exponent);
        let head = expansion.head();
        let Ok(count) = usize::try_from(count(head.point)) else {
            return Rounded::ZERO;
        };
        let mut rounded = Rounded {
            start: head.zeros,
            point: head.point,
            ..Rounded::ZERO
        };
        let end = head.zeros + count;
        let targets = &mut rounded.characters.as_chunks_mut().0[..=end / BLOCK_DIGITS];
        for (target, block) in targets.iter_mut().zip(once(head.block).chain(expansion)) {
            *target = block;
        }
        rounded.round_at(end);
        rounded
    }

    fn round_at(&mut self, end: usize) {
        let rounds_up = self.characters[end] >= b'5';
        self.characters[end..].fill(b'0');
        if !rounds_up {
            return;
        }
        for character in self.characters[self.start..end].iter_mut().rev() {
            if *character != b'9' {
                *character += 1;
                return;
            }
            *character = b'0';
        }
        self.characters[self.start] = b'1';
        self.point += 1;
    }

    /// The first `count` digits.
    pub(crate) fn digits(&self, count: usize) -> &[u8] {
        &self.characters[self.start..self.start + count]
    }
}

/// An integer of `count` digits, and how many of them precede the point. See Cox, "Floating-point printing and parsing
/// can be simple and fast", for why 128 bits of the power are enough.
fn scaled_digits(value: f64, count: usize) -> (u64, i32) {
    let (significand, exponent) = decompose(value);
    let leading_zeros = significand.leading_zeros();
    let top_bit = exponent + 63 - leading_zeros as i32;
    // The value has this many digits before the point, or one more.
    let mut point = floor_log10_of_power_of_two(top_bit) + 1;
    let n = count as i32 - point;
    let power = power_of_ten(n);
    let normalized = u128::from(significand << leading_zeros);
    let product =
        normalized * u128::from(power.high) + ((normalized * u128::from(power.low)) >> 64);
    let doubled = (product >> 64) as u64 >> (61 - top_bit - floor_log2_of_power_of_ten(n));
    let limit = SMALL_POWERS[count];
    let mut digits = doubled.div_ceil(2);
    if doubled >= 2 * limit {
        digits = (doubled / 2 + 5) / 10;
        point += 1;
    }
    if digits == limit {
        digits /= 10;
        point += 1;
    }
    (digits, point)
}
