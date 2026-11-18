use crate::NUMBER_TEXT_CAPACITY;
use crate::digit_words::{eight_digits, split_sixteen_digits, spread_digits, store, to_characters};
use crate::powers_of_ten::SMALL_POWERS;

const EIGHT_DIGITS: u64 = 100_000_000;
const SIXTEEN_DIGITS: u64 = EIGHT_DIGITS * EIGHT_DIGITS;

/// How many digits `number` has.
#[inline]
pub fn decimal_length(number: u64) -> usize {
    let bits = 64 - (number | 1).leading_zeros();
    let at_least = ((bits * 1233) >> 12) as usize;
    at_least + usize::from(number | 1 >= SMALL_POWERS[at_least])
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_i32(number: i32, out: &mut [u8; NUMBER_TEXT_CAPACITY]) -> usize {
    write_integer(number < 0, u64::from(number.unsigned_abs()), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_u32(number: u32, out: &mut [u8; NUMBER_TEXT_CAPACITY]) -> usize {
    write_integer(false, u64::from(number), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_i64(number: i64, out: &mut [u8; NUMBER_TEXT_CAPACITY]) -> usize {
    write_integer(number < 0, number.unsigned_abs(), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_u64(number: u64, out: &mut [u8; NUMBER_TEXT_CAPACITY]) -> usize {
    write_integer(false, number, out)
}

#[inline(always)]
pub(crate) fn write_integer(
    is_negative: bool,
    magnitude: u64,
    out: &mut [u8; NUMBER_TEXT_CAPACITY],
) -> usize {
    out[0] = b'-';
    let start = usize::from(is_negative);
    if magnitude < EIGHT_DIGITS {
        return write_leading(magnitude as u32, out, start);
    }
    write_long(magnitude, out, start)
}

fn write_long(magnitude: u64, out: &mut [u8; NUMBER_TEXT_CAPACITY], start: usize) -> usize {
    if magnitude < SIXTEEN_DIGITS {
        let (upper, lower) = split_sixteen_digits(magnitude);
        let at = write_leading(upper, out, start);
        store(out, at, eight_digits(lower));
        return at + 8;
    }
    let (middle, lower) = split_sixteen_digits(magnitude % SIXTEEN_DIGITS);
    let at = write_leading((magnitude / SIXTEEN_DIGITS) as u32, out, start);
    store(out, at, eight_digits(middle));
    store(out, at + 8, eight_digits(lower));
    at + 16
}

/// A number below 10^8 without leading zeros. Returns where it ends.
#[inline(always)]
fn write_leading(number: u32, out: &mut [u8; NUMBER_TEXT_CAPACITY], at: usize) -> usize {
    let spread = spread_digits(number);
    let unused_bits = (spread | 1).leading_zeros() & !7;
    store(out, at, to_characters(spread) >> unused_bits);
    at + 8 - (unused_bits / 8) as usize
}
