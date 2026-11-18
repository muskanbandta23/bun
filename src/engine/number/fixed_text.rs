//! toFixed, toExponential and toPrecision of Number.prototype.

use crate::digit_words::{ZEROS, eight_digits, store};
use crate::double::decompose;
use crate::integer_text::write_integer;
use crate::powers_of_ten::SMALL_POWERS;
use crate::rounded::Rounded;
use crate::shortest_text::{write_exponent, write_f64_with_exponent};
use crate::{FormatError, NUMBER_TEXT_CAPACITY, NumberText, write_f64};

const CAPACITY: usize = 128;
type Buffer = [u8; CAPACITY];

/// Has room for 100 digits after the point.
pub type FixedText = NumberText<CAPACITY>;

pub fn to_fixed(value: f64, fraction_digits: i32) -> Result<FixedText, FormatError> {
    let Ok(count @ 0..=100) = usize::try_from(fraction_digits) else {
        return Err(FormatError::FixedDigits);
    };
    Ok(FixedText::written_forward(|out| {
        if value.abs() < (1u64 << 53) as f64 && count <= 8 {
            write_few_fraction_digits(value, count, short(out))
        } else if value.abs() < 1e21 {
            let rounded = Rounded::to_fraction_digits(value, count);
            write_positional(value < 0.0, &rounded, count, out)
        } else {
            write_f64(value, short(out))
        }
    }))
}

/// Without `fraction_digits`, as many as it takes to identify the value.
pub fn to_exponential(value: f64, fraction_digits: Option<i32>) -> Result<FixedText, FormatError> {
    let Some(fraction_digits) = fraction_digits.filter(|_| value.is_finite()) else {
        return Ok(FixedText::written_forward(|out| {
            write_f64_with_exponent(value, short(out))
        }));
    };
    let Ok(count @ 0..=100) = usize::try_from(fraction_digits) else {
        return Err(FormatError::ExponentialDigits);
    };
    Ok(FixedText::written_forward(|out| {
        let rounded = Rounded::to_significant_digits(value, count + 1);
        write_exponential(value < 0.0, &rounded, count, out)
    }))
}

pub fn to_precision(value: f64, precision: i32) -> Result<FixedText, FormatError> {
    if !value.is_finite() {
        return Ok(FixedText::written_forward(|out| {
            write_f64(value, short(out))
        }));
    }
    let Ok(count @ 1..=100) = usize::try_from(precision) else {
        return Err(FormatError::Precision);
    };
    Ok(FixedText::written_forward(|out| {
        let rounded = Rounded::to_significant_digits(value, count);
        if (-5..=precision).contains(&rounded.point) {
            let fraction_digits = (precision - rounded.point) as usize;
            write_positional(value < 0.0, &rounded, fraction_digits, out)
        } else {
            write_exponential(value < 0.0, &rounded, count - 1, out)
        }
    }))
}

#[inline(always)]
fn short(out: &mut Buffer) -> &mut [u8; NUMBER_TEXT_CAPACITY] {
    &mut out.as_chunks_mut().0[0]
}

/// For a value below 2^53 and up to eight digits.
fn write_few_fraction_digits(
    value: f64,
    count: usize,
    out: &mut [u8; NUMBER_TEXT_CAPACITY],
) -> usize {
    let (significand, exponent) = decompose(value);
    let places = exponent.unsigned_abs();
    let whole = significand.unbounded_shr(places);
    let fraction = significand - whole.unbounded_shl(places);
    let doubled = u128::from(fraction) * u128::from(2 * SMALL_POWERS[count]);
    let digits = (doubled.unbounded_shr(places) as u64).div_ceil(2);
    let carries = digits == SMALL_POWERS[count];
    let point = write_integer(value < 0.0, whole + u64::from(carries), out);
    if count == 0 {
        return point;
    }
    let characters = if carries {
        ZEROS
    } else {
        eight_digits(digits as u32) >> (64 - 8 * count)
    };
    out[point] = b'.';
    store(out, point + 1, characters);
    point + 1 + count
}

struct Cursor<'a> {
    out: &'a mut Buffer,
    at: usize,
}

impl Cursor<'_> {
    fn after_sign(is_negative: bool, out: &mut Buffer) -> Cursor<'_> {
        out[0] = b'-';
        Cursor {
            out,
            at: usize::from(is_negative),
        }
    }

    fn push(&mut self, characters: &[u8]) {
        self.out[self.at..self.at + characters.len()].copy_from_slice(characters);
        self.at += characters.len();
    }

    /// Nothing if no digits follow the point.
    fn push_fraction(&mut self, zeros: usize, digits: &[u8]) {
        if zeros + digits.len() != 0 {
            self.push(b".");
            self.out[self.at..self.at + zeros].fill(b'0');
            self.at += zeros;
            self.push(digits);
        }
    }
}

fn write_positional(
    is_negative: bool,
    rounded: &Rounded,
    fraction_digits: usize,
    out: &mut Buffer,
) -> usize {
    let mut cursor = Cursor::after_sign(is_negative, out);
    match usize::try_from(rounded.point) {
        Ok(point @ 1..) => {
            let (whole, fraction) = rounded.digits(point + fraction_digits).split_at(point);
            cursor.push(whole);
            cursor.push_fraction(0, fraction);
        }
        _ => {
            let zeros = rounded.point.unsigned_abs() as usize;
            cursor.push(b"0");
            cursor.push_fraction(zeros, rounded.digits(fraction_digits - zeros));
        }
    }
    cursor.at
}

fn write_exponential(
    is_negative: bool,
    rounded: &Rounded,
    fraction_digits: usize,
    out: &mut Buffer,
) -> usize {
    let (first, rest) = rounded.digits(1 + fraction_digits).split_at(1);
    let mut cursor = Cursor::after_sign(is_negative, out);
    cursor.push(first);
    cursor.push_fraction(0, rest);
    write_exponent(rounded.point - 1, cursor.out, cursor.at)
}
