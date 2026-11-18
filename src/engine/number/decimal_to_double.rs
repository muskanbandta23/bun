//! The double closest to a decimal number.

use core::cmp::Ordering;

use crate::double::{EXPONENT_BIAS, FRACTION_BITS, MAX_BIASED_EXPONENT, decompose};
use crate::expansion::Expansion;
use crate::powers_of_ten::{MIN_POWER, floor_log2_of_power_of_ten, power_of_ten};

/// So many digits always fit in a `u64`.
pub(crate) const MAX_SHORT_DIGITS: usize = 19;

static EXACT_POWERS: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// `mantissa * 10^exponent`, where `mantissa` has at most `digit_count` digits.
#[inline(always)]
pub(crate) fn from_short_decimal(mantissa: u64, exponent: i64, digit_count: usize) -> f64 {
    // Clinger, "How to read floating point numbers accurately": one operation on exact operands rounds once.
    if digit_count <= 15 && (-22..=22).contains(&exponent) {
        let mantissa = mantissa as i64 as f64;
        let power = EXACT_POWERS[exponent.unsigned_abs() as usize];
        return if exponent < 0 {
            mantissa / power
        } else {
            mantissa * power
        };
    }
    by_scaling(mantissa, exponent)
}

/// After Lemire, "Number parsing at a gigabyte per second", and Mushtak and Lemire, "Fast number parsing without
/// fallback": 128 bits of the power decide the rounding of any 64-bit mantissa.
#[inline(always)]
fn by_scaling(mantissa: u64, exponent: i64) -> f64 {
    /// Below the 53 bits of the result, the bit that rounds it and the top bit, which may be clear.
    const SPARE_MASK: u64 = (1 << 9) - 1;
    if mantissa == 0 || exponent < i64::from(MIN_POWER) {
        return 0.0;
    }
    if exponent > i64::from(f64::MAX_10_EXP) {
        return f64::INFINITY;
    }
    let exponent = exponent as i32;
    let power = power_of_ten(exponent);
    let leading_zeros = mantissa.leading_zeros();
    let normalized = u128::from(mantissa << leading_zeros);
    let mut product = normalized * u128::from(power.high);
    let mut is_inexact = true;
    // This product is short by less than 2^64: that only matters if it can carry, or if nothing may be left over.
    if ((product >> 64) as u64).wrapping_add(1) & SPARE_MASK <= 1 {
        product += (normalized * u128::from(power.low)) >> 64;
        is_inexact = (product >> 64) as u64 & SPARE_MASK != 0 || product as u64 != 0;
    }
    let top = (product >> 64) as u64;
    let top_bit = (top >> 63) as i32;
    let biased =
        EXPONENT_BIAS + 63 + top_bit + floor_log2_of_power_of_ten(exponent) - leading_zeros as i32;
    if biased >= MAX_BIASED_EXPONENT as i32 {
        return f64::INFINITY;
    }
    let lost_to_underflow = (1 - biased).max(0);
    // The significand, the bit for one half, and a bit for anything less.
    let shift = (8 + top_bit + lost_to_underflow).min(63);
    let unrounded = top >> shift | u64::from(is_inexact);
    let significand = (unrounded + 1 + (unrounded >> 2 & 1)) >> 2;
    // The implicit bit, and a carry out of the significand, add to the exponent.
    f64::from_bits((((biased - 1).max(0) as u64) << FRACTION_BITS) + significand)
}

/// `0.d1d2d3... * 10^point` for any number of digit values.
#[cold]
pub(crate) fn from_long_decimal(digits: impl Iterator<Item = u8> + Clone, point: i64) -> f64 {
    let mut digits = digits.peekable();
    let mut point = point;
    while digits.next_if_eq(&0).is_some() {
        point -= 1;
    }
    let (mantissa, count) = digits
        .clone()
        .take(MAX_SHORT_DIGITS)
        .fold((0, 0), |(mantissa, count), digit| {
            (mantissa * 10 + u64::from(digit), count + 1)
        });
    let below = by_scaling(mantissa, point - count);
    if count < MAX_SHORT_DIGITS as i64 {
        return below;
    }
    let above = by_scaling(mantissa + 1, point - count);
    if below == above {
        return below;
    }
    let (significand, exponent) = decompose(below);
    let halfway = Expansion::new(2 * significand + 1, exponent - 1);
    match compare(digits, point, halfway) {
        Ordering::Less => below,
        Ordering::Equal if significand % 2 == 0 => below,
        _ => above,
    }
}

/// `digits` start with a nonzero one.
fn compare(mut digits: impl Iterator<Item = u8>, point: i64, expansion: Expansion) -> Ordering {
    let mut expected = expansion.digits();
    if point != i64::from(expected.point()) {
        return point.cmp(&i64::from(expected.point()));
    }
    for expected in expected.by_ref() {
        let ordering = digits.next().unwrap_or(0).cmp(&expected);
        if ordering.is_ne() {
            return ordering;
        }
    }
    if digits.any(|digit| digit != 0) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}
