//! Conversions between numeric types (ECMA-262, Type Conversion).

use crate::double::{FRACTION_MASK, IMPLICIT_BIT, SIGNIFICAND_BIAS, biased_exponent};

/// The sum of this and an integer below 2^51 in magnitude has that integer, in two's complement, in its low bits.
const INTEGER_IN_LOW_BITS: f64 = (3u64 << 51) as f64;

/// The integer part of `value` modulo 2^64. Zero if it is not finite.
#[inline]
fn wrapping_truncate(value: f64) -> u64 {
    let bits = value.to_bits();
    let significand = bits & FRACTION_MASK | IMPLICIT_BIT;
    let exponent = biased_exponent(bits) as i32 - SIGNIFICAND_BIAS;
    let magnitude = if exponent < 0 {
        significand.unbounded_shr(exponent.unsigned_abs())
    } else {
        significand.unbounded_shl(exponent as u32)
    };
    if value.is_sign_negative() {
        magnitude.wrapping_neg()
    } else {
        magnitude
    }
}

#[inline]
pub fn to_int32(value: f64) -> i32 {
    if value.abs() < (1u64 << 51) as f64 {
        return (value.trunc() + INTEGER_IN_LOW_BITS).to_bits() as i32;
    }
    wrapping_truncate(value) as i32
}

#[inline]
pub fn to_uint32(value: f64) -> u32 {
    to_int32(value) as u32
}

/// The `i32` that is the same number. There is none for -0.
#[inline]
pub fn exact_int32(value: f64) -> Option<i32> {
    let candidate = (value + INTEGER_IN_LOW_BITS).to_bits() as i32;
    (f64::from(candidate).to_bits() == value.to_bits()).then_some(candidate)
}
