use bun_engine_text::{CodeUnit, Text};

use crate::decimal_to_double::{MAX_SHORT_DIGITS, from_long_decimal, from_short_decimal};
use crate::digit_words::{ZEROS, gather_digits, load, non_digits};
use crate::double::with_sign;
use crate::powers_of_ten::SMALL_POWERS;

/// The value and the length of the longest prefix that is a StrDecimalLiteral of ECMA-262 other than Infinity: an
/// optional sign, digits with an optional point, and an optional exponent.
#[inline]
pub fn parse_decimal_prefix<T: CodeUnit>(units: &[T]) -> Option<(f64, usize)> {
    fn latin1(units: &[u8]) -> (f64, usize) {
        signed_decimal(units)
    }
    fn utf16(units: &[u16]) -> (f64, usize) {
        signed_decimal(units)
    }
    let (value, length) = match T::text(units) {
        Text::Latin1(units) => latin1(units),
        Text::Utf16(units) => utf16(units),
    };
    (length != 0).then_some((value, length))
}

/// The length is zero if there is no number.
#[inline(always)]
fn signed_decimal<T: CodeUnit>(units: &[T]) -> (f64, usize) {
    let (is_negative, start) = scan_sign(units, 0);
    let (point, whole) = scan_few_digits(units, start, 0);
    match finish_decimal(units, start, point, whole) {
        Some((magnitude, end)) => (with_sign(magnitude, is_negative), end),
        None => (0.0, 0),
    }
}

/// Whether there is a minus sign at `at`, and where what follows the sign starts.
#[inline(always)]
pub(crate) fn scan_sign<T: CodeUnit>(units: &[T], at: usize) -> (bool, usize) {
    match units.get(at) {
        Some(unit) => (
            unit.is(b'-'),
            at + usize::from(unit.is(b'-') | unit.is(b'+')),
        ),
        None => (false, at),
    }
}

/// Appends the digits from `at` to `value`, which wraps around, and returns where they end.
#[inline(always)]
pub(crate) fn scan_few_digits<T: CodeUnit>(
    units: &[T],
    mut at: usize,
    mut value: u64,
) -> (usize, u64) {
    while let Some(unit) = units.get(at)
        && unit.is_ascii_digit()
    {
        value = value
            .wrapping_mul(10)
            .wrapping_add(u64::from(unit.to_u16() - u16::from(b'0')));
        at += 1;
    }
    (at, value)
}

/// `scan_few_digits`, eight at a time.
#[inline(always)]
fn scan_many_digits<T: CodeUnit>(units: &[T], mut at: usize, mut value: u64) -> (usize, u64) {
    while let Some(chunk) = units.get(at..).and_then(<[T]>::first_chunk) {
        let characters = load(chunk);
        let digits = characters.wrapping_sub(ZEROS);
        let stop = non_digits(characters);
        if stop != 0 {
            let count = stop.trailing_zeros() / 8;
            // In two steps, for `count` may be zero.
            let digits = digits << (56 - 8 * count) << 8;
            let value = value.wrapping_mul(SMALL_POWERS[count as usize]);
            return (
                at + count as usize,
                value.wrapping_add(gather_digits(digits)),
            );
        }
        value = value
            .wrapping_mul(100_000_000)
            .wrapping_add(gather_digits(digits));
        at += 8;
    }
    scan_few_digits(units, at, value)
}

/// The exponent part at `at`, if there is a complete one, and where it ends.
#[inline(always)]
pub(crate) fn scan_exponent<T: CodeUnit>(units: &[T], at: usize) -> (i64, usize) {
    /// More than the digits of any text can offset.
    const LIMIT: i64 = 1 << 56;
    if !units
        .get(at)
        .is_some_and(|unit| unit.to_u16() | 0x20 == u16::from(b'e'))
    {
        return (0, at);
    }
    let (is_negative, start) = scan_sign(units, at + 1);
    let mut end = start;
    let mut exponent = 0;
    while let Some(digit) = units.get(end).and_then(|unit| unit.digit_value(10)) {
        exponent = (exponent * 10 + i64::from(digit)).min(LIMIT);
        end += 1;
    }
    if end == start {
        return (0, at);
    }
    (if is_negative { -exponent } else { exponent }, end)
}

/// Goes on with a number whose digits from `start` to `point` have been read as `whole`. Returns its value and its
/// end, or nothing if it has no digits.
#[inline(always)]
pub(crate) fn finish_decimal<T: CodeUnit>(
    units: &[T],
    start: usize,
    point: usize,
    whole: u64,
) -> Option<(f64, usize)> {
    let (mut mantissa, mut fraction_start, mut fraction_end) = (whole, point, point);
    if units.get(point).is_some_and(|unit| unit.is(b'.')) {
        fraction_start += 1;
        (fraction_end, mantissa) = scan_many_digits(units, fraction_start, whole);
    }
    let fraction_count = fraction_end - fraction_start;
    let digit_count = point - start + fraction_count;
    if digit_count == 0 {
        return None;
    }
    let (exponent, end) = scan_exponent(units, fraction_end);
    if digit_count > MAX_SHORT_DIGITS {
        let (whole, fraction) = (&units[start..point], &units[fraction_start..fraction_end]);
        return Some((long_decimal(whole, fraction, exponent), end));
    }
    let exponent = exponent - fraction_count as i64;
    Some((from_short_decimal(mantissa, exponent, digit_count), end))
}

#[cold]
#[inline(never)]
pub(crate) fn long_decimal<T: CodeUnit>(whole: &[T], fraction: &[T], exponent: i64) -> f64 {
    let digits = whole.iter().chain(fraction);
    from_long_decimal(
        digits.map(|digit| digit.to_u16() as u8 - b'0'),
        whole.len() as i64 + exponent,
    )
}
