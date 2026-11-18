use crate::NUMBER_TEXT_CAPACITY;
use crate::digit_words::{ZEROS, split_sixteen_digits, spread_digits, store, to_characters};
use crate::double::{
    FRACTION_MASK, IMPLICIT_BIT, MAX_BIASED_EXPONENT, MIN_EXPONENT, SIGN_BIT, SIGNIFICAND_BIAS,
    biased_exponent,
};
use crate::shortest::{Shortest, between_equal_gaps, of_power_of_two};

type Buffer = [u8; NUMBER_TEXT_CAPACITY];

/// Number::toString of ECMA-262 in radix 10. Returns the length of the text, which starts the buffer.
pub fn write_f64(value: f64, out: &mut Buffer) -> usize {
    let bits = value.to_bits();
    let (biased, fraction) = (biased_exponent(bits), bits & FRACTION_MASK);
    if biased.wrapping_sub(1) >= MAX_BIASED_EXPONENT - 1 || fraction == 0 {
        return write_uncommon(value, out);
    }
    let shortest = between_equal_gaps(fraction | IMPLICIT_BIT, biased as i32 - SIGNIFICAND_BIAS);
    write_shortest(bits & SIGN_BIT != 0, shortest, out)
}

#[cold]
#[inline(never)]
fn write_uncommon(value: f64, out: &mut Buffer) -> usize {
    let bits = value.to_bits();
    let (biased, fraction) = (biased_exponent(bits), bits & FRACTION_MASK);
    let is_negative = bits & SIGN_BIT != 0;
    let name: &[u8] = match (biased, fraction) {
        (0, 0) => b"0",
        (0, _) => {
            let shortest = between_equal_gaps(fraction, MIN_EXPONENT).padded();
            return write_shortest(is_negative, shortest, out);
        }
        (MAX_BIASED_EXPONENT, 0) if is_negative => b"-Infinity",
        (MAX_BIASED_EXPONENT, 0) => b"Infinity",
        (MAX_BIASED_EXPONENT, _) => b"NaN",
        _ => {
            let shortest = of_power_of_two(biased as i32 - SIGNIFICAND_BIAS);
            return write_shortest(is_negative, shortest, out);
        }
    };
    out[..name.len()].copy_from_slice(name);
    name.len()
}

/// Seventeen characters, which start with a nonzero digit.
struct Digits {
    first: u64,
    second: u64,
    last: u8,
    /// Without the trailing zeros.
    count: usize,
    /// How many digits precede the decimal point.
    point: i32,
}

impl Digits {
    #[inline(always)]
    fn new(shortest: Shortest) -> Digits {
        let is_short = shortest.leading < 1_000_000_000_000_000;
        let (leading, last) = if is_short {
            (shortest.leading * 10 + shortest.last, 0)
        } else {
            (shortest.leading, shortest.last)
        };
        let (upper, lower) = split_sixteen_digits(leading);
        let (upper, lower) = (spread_digits(upper), spread_digits(lower));
        let trailing_zeros = (u128::from(upper) << 64 | u128::from(lower)).trailing_zeros() / 8;
        Digits {
            first: to_characters(upper),
            second: to_characters(lower),
            last: b'0' + last as u8,
            count: if last != 0 {
                17
            } else {
                16 - trailing_zeros as usize
            },
            point: shortest.exponent + 17 - i32::from(is_short),
        }
    }

    #[inline(always)]
    fn store(&self, out: &mut Buffer, at: usize) {
        store(out, at, self.first);
        store(out, at + 8, self.second);
        out[at + 16] = self.last;
    }
}

#[inline(always)]
fn write_shortest(is_negative: bool, shortest: Shortest, out: &mut Buffer) -> usize {
    let digits = Digits::new(shortest);
    out[0] = b'-';
    let start = usize::from(is_negative);
    match digits.point {
        1..=16 => write_with_point(&digits, out, start),
        -5..=0 => {
            let at = start + (2 - digits.point) as usize;
            store(out, start, u64::from_le_bytes(*b"0.000000"));
            digits.store(out, at);
            at + digits.count
        }
        17..=21 => {
            digits.store(out, start);
            store(out, start + 17, ZEROS);
            start + digits.point as usize
        }
        _ => write_exponential(&digits, out, start),
    }
}

#[inline(always)]
fn write_with_point(digits: &Digits, out: &mut Buffer, start: usize) -> usize {
    /// The first `count` bytes of `word`, then all of it one byte up.
    #[inline(always)]
    fn with_gap(word: u64, count: usize) -> u64 {
        let moved = u64::MAX << (count * 8);
        word & !moved | (word << 8) & moved
    }
    let point = digits.point as usize;
    digits.store(out, start + 1);
    if point < 8 {
        store(out, start, with_gap(digits.first, point));
    } else {
        store(out, start, digits.first);
        let second = match point {
            16 => digits.second,
            _ => with_gap(digits.second, point - 8),
        };
        store(out, start + 8, second);
    }
    out[start + point] = b'.';
    start
        + if digits.count > point {
            digits.count + 1
        } else {
            point
        }
}

#[inline(always)]
fn write_exponential(digits: &Digits, out: &mut Buffer, start: usize) -> usize {
    digits.store(out, start + 1);
    out[start] = digits.first as u8;
    out[start + 1] = b'.';
    let at = start + digits.count + usize::from(digits.count > 1);
    write_exponent(digits.point - 1, out, at)
}

/// `e`, a sign and up to three digits in a word that goes at `at`. Returns where the text ends.
#[inline(always)]
pub(crate) fn write_exponent<const N: usize>(exponent: i32, out: &mut [u8; N], at: usize) -> usize {
    let magnitude = exponent.unsigned_abs();
    let (hundreds, rest) = (magnitude / 100, magnitude % 100);
    let characters = u64::from(hundreds | (rest / 10) << 8 | (rest % 10) << 16) | 0x30_3030;
    let count = 1 + usize::from(magnitude >= 10) + usize::from(magnitude >= 100);
    let sign = if exponent < 0 { b'-' } else { b'+' };
    let word = u64::from(b'e') | u64::from(sign) << 8 | characters >> (24 - 8 * count) << 16;
    store(out, at, word);
    at + 2 + count
}
