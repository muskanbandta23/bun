use crate::NUMBER_TEXT_CAPACITY;
use crate::digit_words::{eight_digits, split_sixteen_digits, store};
use crate::powers_of_ten::SMALL_POWERS;

type Buffer = [u8; NUMBER_TEXT_CAPACITY];

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
pub fn write_i32(number: i32, out: &mut Buffer) -> usize {
    write_integer(number < 0, u64::from(number.unsigned_abs()), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_u32(number: u32, out: &mut Buffer) -> usize {
    write_integer(false, u64::from(number), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_i64(number: i64, out: &mut Buffer) -> usize {
    write_integer(number < 0, number.unsigned_abs(), out)
}

/// Returns the length of the text, which starts the buffer.
#[inline]
pub fn write_u64(number: u64, out: &mut Buffer) -> usize {
    write_integer(false, number, out)
}

#[inline(always)]
pub(crate) fn write_integer(is_negative: bool, magnitude: u64, out: &mut Buffer) -> usize {
    if magnitude < EIGHT_DIGITS {
        return write_short(is_negative, magnitude as u32, out);
    }
    write_long(is_negative, magnitude, out)
}

fn write_long(is_negative: bool, magnitude: u64, out: &mut Buffer) -> usize {
    if magnitude < SIXTEEN_DIGITS {
        let (upper, lower) = split_sixteen_digits(magnitude);
        let at = write_short(is_negative, upper, out);
        store(out, at, eight_digits(lower));
        return at + 8;
    }
    let (middle, lower) = split_sixteen_digits(magnitude % SIXTEEN_DIGITS);
    let at = write_short(is_negative, (magnitude / SIXTEEN_DIGITS) as u32, out);
    store(out, at, eight_digits(middle));
    store(out, at + 8, eight_digits(lower));
    at + 16
}

/// A number below 10^8.
#[inline(always)]
fn write_short(is_negative: bool, magnitude: u32, out: &mut Buffer) -> usize {
    let start = usize::from(is_negative);
    if magnitude < 10 {
        out[0] = b'-';
        out[start] = b'0' + magnitude as u8;
        return start + 1;
    }
    if magnitude < 10_000 {
        return write_up_to_four_digits(magnitude, out, start);
    }
    let lower = four_digits(magnitude % 10_000);
    let at = write_up_to_four_digits(magnitude / 10_000, out, start);
    out[at..at + 4].copy_from_slice(&lower.to_le_bytes());
    at + 4
}

#[inline(always)]
fn write_up_to_four_digits(number: u32, out: &mut Buffer, start: usize) -> usize {
    let unused = u32::from(number < 10) + u32::from(number < 100) + u32::from(number < 1000);
    let characters = four_digits(number) >> (unused * 8);
    out[0] = b'-';
    out[start..start + 4].copy_from_slice(&characters.to_le_bytes());
    start + 4 - unused as usize
}

#[inline(always)]
fn four_digits(number: u32) -> u32 {
    static PAIRS: [u16; 128] = {
        let mut pairs = [0; 128];
        let mut n = 0;
        while n < 100 {
            pairs[n as usize] = u16::from_le_bytes([b'0' + n / 10, b'0' + n % 10]);
            n += 1;
        }
        pairs
    };
    let upper = (number * 5243) >> 19;
    let pair = |n: u32| u32::from(PAIRS[n as usize % PAIRS.len()]);
    pair(upper) | pair(number - upper * 100) << 16
}
