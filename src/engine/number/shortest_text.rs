use core::hint::select_unpredictable;

use crate::NUMBER_TEXT_CAPACITY;
use crate::digit_words::{ZEROS, store};
use crate::double::{
    FRACTION_MASK, IMPLICIT_BIT, MAX_BIASED_EXPONENT, SIGN_BIT, SIGNIFICAND_BIAS, biased_exponent,
};
use crate::shortest::{Shortest, between_equal_gaps};
use crate::sixteen_digits::write_sixteen_digits;

type Buffer = [u8; NUMBER_TEXT_CAPACITY];

/// Number::toString of ECMA-262 in radix 10. Returns the length of the text, which starts the buffer.
pub fn write_f64(value: f64, out: &mut Buffer) -> usize {
    let bits = value.to_bits();
    let (biased, significand) = (biased_exponent(bits), bits & FRACTION_MASK | IMPLICIT_BIT);
    if biased.wrapping_sub(1) < MAX_BIASED_EXPONENT - 1
        && bits & FRACTION_MASK != 0
        && let Some(shortest) =
            between_equal_gaps::<true>(significand, biased as i32 - SIGNIFICAND_BIAS)
    {
        return write_shortest(bits & SIGN_BIT != 0, shortest, out);
    }
    write_uncommon(value, out)
}

#[cold]
#[inline(never)]
fn write_uncommon(value: f64, out: &mut Buffer) -> usize {
    if let Some(shortest) = Shortest::of(value) {
        return write_shortest(value < 0.0, shortest, out);
    }
    let name: &[u8] = match value {
        0.0 => b"0",
        f64::INFINITY => b"Infinity",
        f64::NEG_INFINITY => b"-Infinity",
        _ => b"NaN",
    };
    out[..name.len()].copy_from_slice(name);
    name.len()
}

/// The digits of `write_f64`, always with an exponent if the value is finite.
pub(crate) fn write_f64_with_exponent(value: f64, out: &mut Buffer) -> usize {
    match Shortest::of(value) {
        Some(shortest) => {
            let (leading, last, point) = split(shortest);
            write_exponential(out, value < 0.0, leading, last, point)
        }
        None if value == 0.0 => {
            out[0] = b'0';
            write_exponent(0, out, 1)
        }
        None => write_uncommon(value, out),
    }
}

/// The digit that follows the 15 or 16 leading ones.
#[derive(Clone, Copy)]
struct LastDigit {
    character: u8,
    is_sixteenth: bool,
    is_needed: bool,
}

impl LastDigit {
    #[inline(always)]
    fn index(self) -> usize {
        16 - usize::from(self.is_sixteenth)
    }

    /// How many digits there are up to the last nonzero one.
    #[inline(always)]
    fn count(self, leading_count: usize) -> usize {
        select_unpredictable(self.is_needed, self.index() + 1, leading_count)
    }
}

/// Sixteen digits from the first nonzero one, the last digit, and how many digits precede the decimal point.
#[inline(always)]
fn split(shortest: Shortest) -> (u64, LastDigit, i32) {
    let is_short = shortest.leading < 1_000_000_000_000_000;
    let leading = select_unpredictable(is_short, shortest.leading * 10, shortest.leading);
    let last = LastDigit {
        character: shortest.last,
        is_sixteenth: is_short,
        is_needed: shortest.needs_last,
    };
    (leading, last, shortest.exponent + last.index() as i32 + 1)
}

#[inline(always)]
fn write_shortest(is_negative: bool, shortest: Shortest, out: &mut Buffer) -> usize {
    let (leading, last, point) = split(shortest);
    match point {
        -5..=7 => write_with_early_point(out, is_negative, leading, last, point),
        8..=15 => write_with_late_point(out, is_negative, leading, last, point),
        16..=21 => write_large(out, is_negative, leading, last, point),
        _ => write_exponential(out, is_negative, leading, last, point),
    }
}

#[derive(Clone, Copy)]
#[repr(align(16))]
struct Head {
    /// Selects the digits that do not make room for the point.
    kept: u64,
    kept_at: u8,
    moved_at: u8,
    point_at: u8,
}

static HEADS: [Head; 13] = {
    let mut heads = [Head {
        kept: u64::MAX,
        kept_at: 0,
        moved_at: 1,
        point_at: 1,
    }; 13];
    let mut index = 0;
    while index < 13 {
        if index < 6 {
            heads[index].kept_at = 7 - index as u8;
            heads[index].moved_at = 7 - index as u8;
        } else {
            heads[index].point_at = index as u8 - 5;
            heads[index].kept = (1 << (8 * (index - 5))) - 1;
        }
        index += 1;
    }
    heads
};

#[inline(always)]
fn sixteen_at(out: &mut Buffer, at: usize) -> &mut [u8; 16] {
    match out[at..].first_chunk_mut() {
        Some(chunk) => chunk,
        None => unreachable!(),
    }
}

#[inline(always)]
fn write_with_early_point(
    out: &mut Buffer,
    is_negative: bool,
    leading: u64,
    last: LastDigit,
    point: i32,
) -> usize {
    let start = usize::from(is_negative);
    let head = &HEADS[(point + 5) as usize];
    let moved_at = usize::from(head.moved_at & 7);
    let point_at = usize::from(head.point_at & 7);
    out[0] = b'-';
    store(out, start, ZEROS);
    let written = write_sixteen_digits(leading, sixteen_at(out, start + moved_at));
    out[start + moved_at + last.index()] = last.character;
    let first = written.first_eight;
    let merged = first & head.kept | (first << 8) & !head.kept;
    store(out, start + usize::from(head.kept_at & 7), merged);
    out[start + point_at] = b'.';
    let end = moved_at + last.count(written.count);
    start + select_unpredictable(end > point_at + 1, end, point_at)
}

#[inline(never)]
fn write_with_late_point(
    out: &mut Buffer,
    is_negative: bool,
    leading: u64,
    last: LastDigit,
    point: i32,
) -> usize {
    let start = usize::from(is_negative);
    let point = point as usize & 15;
    out[0] = b'-';
    let written = write_sixteen_digits(leading, sixteen_at(out, start + 1));
    out[start + 1 + last.index()] = last.character;
    let (second, moved) = (written.second_eight, u64::MAX << (point % 8 * 8));
    store(out, start + 8, second & !moved | (second << 8) & moved);
    out[start + point] = b'.';
    store(out, start, written.first_eight);
    let count = last.count(written.count);
    start + select_unpredictable(count > point, count + 1, point)
}

#[inline(never)]
fn write_large(
    out: &mut Buffer,
    is_negative: bool,
    leading: u64,
    last: LastDigit,
    point: i32,
) -> usize {
    let start = usize::from(is_negative);
    out[0] = b'-';
    write_sixteen_digits(leading, sixteen_at(out, start));
    store(out, start + 16, ZEROS);
    out[start + last.index()] = select_unpredictable(last.is_needed, last.character, b'0');
    if last.is_needed && last.index() == point as usize {
        out[start + 16..start + 18].copy_from_slice(&[b'.', last.character]);
        return start + 18;
    }
    start + point as usize
}

#[inline(never)]
fn write_exponential(
    out: &mut Buffer,
    is_negative: bool,
    leading: u64,
    last: LastDigit,
    point: i32,
) -> usize {
    let start = usize::from(is_negative);
    out[0] = b'-';
    let written = write_sixteen_digits(leading, sixteen_at(out, start + 1));
    out[start + 1 + last.index()] = last.character;
    out[start..start + 2].copy_from_slice(&[written.first_eight as u8, b'.']);
    let count = last.count(written.count);
    write_exponent(point - 1, out, start + count + usize::from(count > 1))
}

/// The digits of each magnitude, and how many in the top byte.
static EXPONENTS: [u32; 325] = {
    let mut exponents = [0; 325];
    let mut n = 0;
    while n < 325 {
        let count = 1 + (n >= 10) as u32 + (n >= 100) as u32;
        let digits = (n / 100) | (n / 10 % 10) << 8 | (n % 10) << 16 | 0x30_3030;
        exponents[n as usize] = digits >> (24 - 8 * count) | count << 24;
        n += 1;
    }
    exponents
};

/// `e`, a sign and up to three digits in a word that goes at `at`. Returns where the text ends.
#[inline(always)]
pub(crate) fn write_exponent<const N: usize>(exponent: i32, out: &mut [u8; N], at: usize) -> usize {
    let is_negative = exponent < 0;
    let digits = EXPONENTS[select_unpredictable(is_negative, -exponent, exponent) as usize];
    let prefix = select_unpredictable(is_negative, *b"e-", *b"e+");
    let word = u64::from(u16::from_le_bytes(prefix)) | u64::from(digits) << 16;
    store(out, at, word);
    at + 2 + (digits >> 24) as usize
}
