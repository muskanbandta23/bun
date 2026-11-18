use bun_engine_text::{CodeUnit, Text};

use crate::decimal_scanner::{finish_decimal, scan_whole_digits};
use crate::double::with_sign;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsonNumber {
    Int32(i32),
    /// May be an integer too.
    Double(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonNumberError {
    NoDigits,
    NoDigitsAfterPoint,
}

impl JsonNumberError {
    pub fn message(self) -> &'static str {
        match self {
            JsonNumberError::NoDigits => "Invalid number",
            JsonNumberError::NoDigitsAfterPoint => "Invalid digits after decimal point",
        }
    }
}

/// The value and the length of the number (ECMA-404) that `units` start with. It ends before a digit that follows a
/// leading zero, and before an exponent that has no digits.
#[inline]
pub fn parse_json_number<T: CodeUnit>(units: &[T]) -> Result<(JsonNumber, usize), JsonNumberError> {
    const MAX_INT32_DIGITS: usize = 9;
    let start = usize::from(units.first().is_some_and(|unit| unit.is(b'-')));
    let (point, whole) = match units.get(start) {
        Some(first) if first.is(b'0') => (start + 1, 0),
        Some(first) if first.is_ascii_digit() => scan_whole_digits(units, start),
        _ => return Err(JsonNumberError::NoDigits),
    };
    let goes_on = units
        .get(point)
        .is_some_and(|unit| unit.is(b'.') || unit.to_u16() | 0x20 == u16::from(b'e'));
    if !goes_on && point - start <= MAX_INT32_DIGITS {
        let number = match (start, whole as i32) {
            (0, whole) => JsonNumber::Int32(whole),
            (_, 0) => JsonNumber::Double(-0.0),
            (_, whole) => JsonNumber::Int32(-whole),
        };
        return Ok((number, point));
    }
    let (value, end) = match T::text(units) {
        Text::Latin1(units) => finish_latin1(units, start, point, whole),
        Text::Utf16(units) => finish_utf16(units, start, point, whole),
    };
    if end == 0 {
        return Err(JsonNumberError::NoDigitsAfterPoint);
    }
    Ok((JsonNumber::Double(value), end))
}

fn finish_latin1(units: &[u8], start: usize, point: usize, whole: u64) -> (f64, usize) {
    finish(units, start, point, whole)
}

fn finish_utf16(units: &[u16], start: usize, point: usize, whole: u64) -> (f64, usize) {
    finish(units, start, point, whole)
}

/// The end is zero if no digit follows the point.
#[inline(always)]
fn finish<T: CodeUnit>(units: &[T], start: usize, point: usize, whole: u64) -> (f64, usize) {
    if let Some([first, rest @ ..]) = units.get(point..)
        && first.is(b'.')
        && !rest.first().is_some_and(|unit| unit.is_ascii_digit())
    {
        return (0.0, 0);
    }
    match finish_decimal(units, start, point, whole) {
        Some((magnitude, end)) => (with_sign(magnitude, start != 0), end),
        None => (0.0, 0),
    }
}
