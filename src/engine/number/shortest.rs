//! The shortest decimal that rounds to a double, and of those the closest.
//!
//! As in Giulietti, "The Schubfach way to render doubles": with 10^k the largest power of ten that is no wider than the
//! rounding interval, the interval has at most one multiple of 10^(k+1), which is the result if it is there, and
//! otherwise the result is a multiple of 10^k. Dividing by 10^(k+1) yields both from one product: the candidates for
//! the former are its integer part and the next integer, and the latter takes one more digit from the fraction.

use crate::double::IMPLICIT_BIT;
use crate::integer_text::decimal_length;
use crate::powers_of_ten::{
    SMALL_POWERS, floor_log2_of_power_of_ten, floor_log10_of_power_of_two, power_of_ten,
};

/// `(leading * 10 + last) * 10^exponent`
#[derive(Clone, Copy)]
pub(crate) struct Shortest {
    /// 15 or 16 digits.
    pub(crate) leading: u64,
    pub(crate) last: u64,
    pub(crate) exponent: i32,
}

struct Scaled {
    integer: u64,
    /// In units of 2^-64, like `half_gap`.
    fraction: u64,
    /// Half the distance to the next double.
    half_gap: u64,
}

const HALF: u64 = 1 << 63;

/// `significand * 2^exponent / 10^(k + 1)`, where `2^exponent / 10^(k + 1)` is between 1/16 and 2.
#[inline(always)]
fn scale(significand: u64, exponent: i32, k: i32) -> Scaled {
    const EXTRA_BITS: u32 = 4;
    let power = power_of_ten(-k - 1);
    let shift = (exponent + floor_log2_of_power_of_ten(-k - 1) + 1 + EXTRA_BITS as i32) as u32;
    let shifted = u128::from(significand << shift);
    let product = shifted * u128::from(power.high) + ((shifted * u128::from(power.low)) >> 64);
    Scaled {
        integer: (product >> (64 + EXTRA_BITS)) as u64,
        fraction: (product >> EXTRA_BITS) as u64,
        half_gap: power.high >> (EXTRA_BITS + 1 - shift),
    }
}

/// The integer part of `(fraction * 10 + addend) / 2^64`.
#[inline(always)]
fn first_digit(fraction: u64, addend: u64) -> u64 {
    ((u128::from(fraction) * 10 + u128::from(addend)) >> 64) as u64
}

/// For a value as far from the double below as from the one above.
#[inline(always)]
pub(crate) fn between_equal_gaps(significand: u64, exponent: i32) -> Shortest {
    // Makes up for the bits that `fraction` lacks: with it no double is rounded the wrong way.
    const CORRECTION: u64 = 6;
    let k = floor_log10_of_power_of_two(exponent);
    let scaled = scale(significand, exponent, k);
    // A text halfway between two doubles reads as the even one.
    let reach = scaled.half_gap + (!significand & 1);
    let rounds_down = scaled.fraction < reach;
    let rounds_up = scaled.fraction.overflowing_add(reach).1;
    // The only ties are at 2.5 and 7.5, and go to the even digit.
    let digit = match scaled.fraction {
        fraction if fraction == HALF / 2 => 2,
        fraction => first_digit(fraction, HALF + CORRECTION),
    };
    Shortest {
        leading: scaled.integer + u64::from(rounds_up),
        last: if rounds_down | rounds_up { 0 } else { digit },
        exponent: k,
    }
}

/// For `IMPLICIT_BIT * 2^exponent`: the double below is half as far as the one above. It is not for the smallest normal
/// value, where that makes no difference.
#[cold]
pub(crate) fn of_power_of_two(exponent: i32) -> Shortest {
    // The interval is 3/4 as wide: this is floor(log10(3/4 * 2^exponent)).
    let k = (exponent * 315_653 - 131_008) >> 20;
    let scaled = scale(IMPLICIT_BIT, exponent, k);
    let reach_down = scaled.half_gap / 2;
    let rounds_down = scaled.fraction <= reach_down;
    let rounds_up = scaled.fraction.overflowing_add(scaled.half_gap).1;
    let last = if rounds_down || rounds_up {
        0
    } else {
        let closest = first_digit(scaled.fraction, HALF - 1);
        closest.max(first_digit(scaled.fraction - reach_down, u64::MAX))
    };
    Shortest {
        leading: scaled.integer + u64::from(rounds_up),
        last,
        exponent: k,
    }
}

impl Shortest {
    /// Gives a nonzero result that came out with fewer digits the usual number of them.
    #[cold]
    pub(crate) fn padded(self) -> Shortest {
        let digits = self.leading * 10 + self.last;
        let missing = 17 - decimal_length(digits);
        let digits = digits * SMALL_POWERS[missing];
        Shortest {
            leading: digits / 10,
            last: digits % 10,
            exponent: self.exponent - missing as i32,
        }
    }
}
