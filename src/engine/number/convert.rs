//! Conversions between numeric types (ECMA-262, Type Conversion).

use core::hint::select_unpredictable;

use bun_engine_text::MAX_ARRAY_INDEX;

use crate::double::{FRACTION_MASK, IMPLICIT_BIT, SIGN_BIT, SIGNIFICAND_BIAS, biased_exponent};

/// Number.MAX_SAFE_INTEGER
pub const MAX_SAFE_INTEGER: f64 = ((1u64 << 53) - 1) as f64;

/// The sum of this and an integer below 2^51 in magnitude has that integer, in two's complement, in its low bits.
const INTEGER_IN_LOW_BITS: f64 = (3u64 << 51) as f64;

#[inline]
pub fn to_int32(value: f64) -> i32 {
    if value.abs() < (1u64 << 51) as f64 {
        return (value.trunc() + INTEGER_IN_LOW_BITS).to_bits() as i32;
    }
    to_uint64(value) as i32
}

#[inline]
pub fn to_uint32(value: f64) -> u32 {
    to_int32(value) as u32
}

#[inline]
pub fn to_int16(value: f64) -> i16 {
    to_int32(value) as i16
}

#[inline]
pub fn to_uint16(value: f64) -> u16 {
    to_int32(value) as u16
}

#[inline]
pub fn to_int8(value: f64) -> i8 {
    to_int32(value) as i8
}

#[inline]
pub fn to_uint8(value: f64) -> u8 {
    to_int32(value) as u8
}

/// ToUint8Clamp, which rounds halves to even.
#[inline]
pub fn to_uint8_clamp(value: f64) -> u8 {
    let clamped = if value > 0.0 { value } else { 0.0 };
    let clamped = if clamped < 255.0 { clamped } else { 255.0 };
    (clamped + INTEGER_IN_LOW_BITS).to_bits() as u8
}

/// As ToInt32, modulo 2^64.
#[inline]
pub fn to_int64(value: f64) -> i64 {
    to_uint64(value) as i64
}

/// As ToUint32, modulo 2^64.
#[inline]
pub fn to_uint64(value: f64) -> u64 {
    let bits = value.to_bits();
    let significand = bits & FRACTION_MASK | IMPLICIT_BIT;
    let exponent = biased_exponent(bits) as i32 - SIGNIFICAND_BIAS;
    let magnitude = select_unpredictable(
        exponent < 0,
        significand.unbounded_shr(exponent.wrapping_neg() as u32),
        significand.unbounded_shl(exponent as u32),
    );
    let sign = (bits as i64 >> 63) as u64;
    (magnitude ^ sign).wrapping_sub(sign)
}

#[inline]
pub fn to_integer_or_infinity(value: f64) -> f64 {
    if value.is_nan() {
        return 0.0;
    }
    value.trunc() + 0.0
}

#[inline]
pub fn to_length(value: f64) -> u64 {
    (value as i64).clamp(0, MAX_SAFE_INTEGER as i64) as u64
}

/// A RangeError.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexError {
    Negative,
    TooLarge,
}

impl IndexError {
    /// What follows the name of the argument in the message.
    pub fn message_after_name(self) -> &'static str {
        match self {
            IndexError::Negative => " cannot be negative",
            IndexError::TooLarge => " larger than (2 ** 53) - 1",
        }
    }
}

#[inline]
pub fn to_index(value: f64) -> Result<u64, IndexError> {
    match value as i64 {
        ..0 => Err(IndexError::Negative),
        index if index > MAX_SAFE_INTEGER as i64 => Err(IndexError::TooLarge),
        index => Ok(index as u64),
    }
}

/// The integer part of `value`, kept between zero and `length`.
#[inline]
pub fn to_clamped_index(value: f64, length: u64) -> u64 {
    ((value as i64).max(0) as u64).min(length)
}

/// As `to_clamped_index`, but a negative integer counts from `length`.
#[inline]
pub fn to_relative_index(value: f64, length: u64) -> u64 {
    match value as i64 {
        index @ ..0 => length.saturating_sub(index.unsigned_abs()),
        index => length.min(index as u64),
    }
}

/// The conversion to `[EnforceRange] unsigned long` of Web IDL.
#[inline]
pub fn enforce_uint32_range(value: f64) -> Option<u32> {
    (value > -1.0 && value < (1u64 << 32) as f64).then_some(value as u32)
}

/// The `i32` that is the same number. There is none for -0.
#[inline]
pub fn exact_int32(value: f64) -> Option<i32> {
    let candidate = (value + INTEGER_IN_LOW_BITS).to_bits() as i32;
    (f64::from(candidate).to_bits() == value.to_bits()).then_some(candidate)
}

/// The integer from -2^51 to 2^51 - 1 that is the same number. There is none for -0.
#[inline]
pub fn exact_int52(value: f64) -> Option<i64> {
    let candidate = ((value + INTEGER_IN_LOW_BITS).to_bits() << 12 ^ SIGN_BIT) as i64 >> 12;
    ((candidate as f64).to_bits() == value.to_bits()).then_some(candidate)
}

/// The array index that `value` is as a property key.
#[inline]
pub fn exact_array_index(value: f64) -> Option<u32> {
    let candidate = (value + INTEGER_IN_LOW_BITS).to_bits() as u32;
    (f64::from(candidate) == value && candidate <= MAX_ARRAY_INDEX).then_some(candidate)
}

#[inline]
pub fn is_integer(value: f64) -> bool {
    value - value.trunc() == 0.0
}

/// Number.isSafeInteger
#[inline]
pub fn is_safe_integer(value: f64) -> bool {
    value.trunc() == value && value.abs() <= MAX_SAFE_INTEGER
}

#[inline]
pub fn is_negative_zero(value: f64) -> bool {
    value.to_bits() == SIGN_BIT
}
