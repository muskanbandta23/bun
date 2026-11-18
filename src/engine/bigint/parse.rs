use bun_engine_text::{CodeUnit, Text, with_text};
use bun_engine_unicode::is_str_white_space;

use crate::radix::{CHUNKS, log2_rounded_down};
use crate::{Error, Fill, Filled, MAX_BITS, New, Poll, SmallBigInt, Work, digits};

/// The radices that integer literals are written in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LiteralRadix {
    Binary = 2,
    Octal = 8,
    Decimal = 10,
    Hexadecimal = 16,
}

impl LiteralRadix {
    /// A longer text is too big whatever its value is.
    const fn max_characters(self) -> usize {
        match self {
            LiteralRadix::Binary => MAX_BITS,
            LiteralRadix::Octal => MAX_BITS / 3,
            LiteralRadix::Decimal => (MAX_BITS as u64 * 32 / 107) as usize,
            LiteralRadix::Hexadecimal => MAX_BITS / 4,
        }
    }

    const fn max_small_characters(self) -> usize {
        match self {
            LiteralRadix::Binary => 128,
            LiteralRadix::Octal => 42,
            LiteralRadix::Decimal => 38,
            LiteralRadix::Hexadecimal => 32,
        }
    }

    fn max_bits(self, characters: usize) -> usize {
        const BITS_PER_DECIMAL_DIGIT: u64 = (log2_rounded_down(10) >> 8) + 1;
        match self {
            LiteralRadix::Decimal => {
                ((characters as u64 * BITS_PER_DECIMAL_DIGIT) >> 32) as usize + 1
            }
            _ => characters * (self as u32).ilog2() as usize,
        }
    }
}

/// StringToBigInt of ECMA-262. [`Error::InvalidText`], here or from [`Fill::fill`], is its `undefined`.
#[inline]
pub fn from_string(text: Text<'_>) -> Result<New<impl Fill + '_>, Error> {
    with_text!(text, |units| string_integer_literal(units))
}

/// `characters` have no sign, prefix, suffix or separators.
#[inline]
pub fn from_literal(
    characters: Text<'_>,
    radix: LiteralRadix,
    is_negative: bool,
) -> Result<New<impl Fill + '_>, Error> {
    with_text!(characters, |units| literal(units, radix, is_negative))
}

fn string_integer_literal<T: CodeUnit>(mut units: &[T]) -> Result<New<Literal<'_>>, Error> {
    while let [first, rest @ ..] = units
        && is_str_white_space(first.to_u32())
    {
        units = rest;
    }
    while let [rest @ .., last] = units
        && is_str_white_space(last.to_u32())
    {
        units = rest;
    }
    if let [zero, prefix, rest @ ..] = units
        && zero.is(b'0')
    {
        match u8::try_from(prefix.to_u16() | 0x20) {
            Ok(b'b') => return literal(rest, LiteralRadix::Binary, false),
            Ok(b'o') => return literal(rest, LiteralRadix::Octal, false),
            Ok(b'x') => return literal(rest, LiteralRadix::Hexadecimal, false),
            _ => {}
        }
    }
    match units {
        [] => Ok(New::Small(SmallBigInt::ZERO)),
        [sign, rest @ ..] if sign.is(b'-') => literal(rest, LiteralRadix::Decimal, true),
        [sign, rest @ ..] if sign.is(b'+') => literal(rest, LiteralRadix::Decimal, false),
        _ => literal(units, LiteralRadix::Decimal, false),
    }
}

fn literal<T: CodeUnit>(
    mut units: &[T],
    radix: LiteralRadix,
    is_negative: bool,
) -> Result<New<Literal<'_>>, Error> {
    if units.is_empty() {
        return Err(Error::InvalidText);
    }
    while let [first, rest @ ..] = units
        && first.is(b'0')
    {
        units = rest;
    }
    if units.len() <= radix.max_small_characters() {
        let magnitude = units.iter().try_fold(0u128, |value, unit| {
            Some(value * radix as u128 + u128::from(unit.digit_value(radix as u32)?))
        });
        return match magnitude {
            Some(magnitude) => Ok(New::Small(SmallBigInt::new(is_negative, magnitude))),
            None => Err(Error::InvalidText),
        };
    }
    if units.len() > radix.max_characters() {
        return Err(Error::TooBig);
    }
    if units[0].digit_value(radix as u32).is_none() {
        return Err(Error::InvalidText);
    }
    Ok(New::Large(Literal {
        characters: T::text(units),
        radix,
        is_negative,
    }))
}

struct Literal<'a> {
    /// The first is not zero.
    characters: Text<'a>,
    radix: LiteralRadix,
    is_negative: bool,
}

impl Fill for Literal<'_> {
    #[inline]
    fn digit_count(&self) -> usize {
        self.radix.max_bits(self.characters.len()).div_ceil(64)
    }

    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.digit_count()];
        with_text!(self.characters, |units| match self.radix {
            LiteralRadix::Decimal => read_decimal(units, out, &mut Work::new(poll)),
            radix => read_bit_groups(units, radix as u32, out),
        })?;
        Filled::whole_or_one_less(self.is_negative, out)
    }
}

fn read_bit_groups<T: CodeUnit>(units: &[T], radix: u32, out: &mut [u64]) -> Result<(), Error> {
    let bits = radix.ilog2();
    let mut out = out.iter_mut();
    let (mut pending, mut pending_bits) = (0u128, 0);
    for unit in units.iter().rev() {
        let value = unit.digit_value(radix).ok_or(Error::InvalidText)?;
        pending |= u128::from(value) << pending_bits;
        pending_bits += bits;
        if pending_bits >= 64
            && let Some(out) = out.next()
        {
            *out = pending as u64;
            pending >>= 64;
            pending_bits -= 64;
        }
    }
    if let Some(out) = out.next() {
        *out = pending as u64;
    }
    Ok(())
}

fn read_decimal<T: CodeUnit>(
    units: &[T],
    out: &mut [u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    let chunk = CHUNKS[10];
    let mut length = 0;
    for part in units.rchunks(chunk.characters).rev() {
        work.spend(length)?;
        let value = part
            .iter()
            .try_fold(0, |value, unit| {
                Some(value * 10 + u64::from(unit.digit_value(10)?))
            })
            .ok_or(Error::InvalidText)?;
        let top = digits::multiply_assign_digit(&mut out[..length], chunk.power, value);
        if top != 0 {
            out[length] = top;
            length += 1;
        }
    }
    out[length..].fill(0);
    Ok(())
}
