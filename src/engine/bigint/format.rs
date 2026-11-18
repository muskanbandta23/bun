use bun_engine_text::{StringRef, StringWriter};

use crate::radix::{CHARACTERS_PER_BIT, CHUNKS};
use crate::{BigIntRef, Error, Poll, Scratch, Work, digits};

const CHARACTERS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

impl BigIntRef<'_> {
    /// How much room [`write_text`](Self::write_text) needs. `radix` is from 2 to 36.
    #[inline]
    pub fn max_text_length(self, radix: u32) -> usize {
        let bits = digits::bit_length(self.digits);
        let characters = if radix.is_power_of_two() {
            bits.div_ceil(radix.ilog2() as usize).max(1)
        } else {
            ((bits as u64 * CHARACTERS_PER_BIT[radix as usize]) >> 32) as usize + 1
        };
        characters + usize::from(self.is_negative)
    }

    /// BigInt::toString of ECMA-262. Returns the text, which ends where `out` ends.
    pub fn write_text<'a>(
        self,
        radix: u32,
        out: &'a mut [u8],
        poll: Poll<'_>,
    ) -> Result<&'a [u8], Error> {
        let mut start = if self.is_zero() {
            out[out.len() - 1] = b'0';
            out.len() - 1
        } else if radix.is_power_of_two() {
            write_bit_groups(self.digits, radix.ilog2(), out)
        } else {
            write_by_division(self.digits, radix, out, &mut Work::new(poll))?
        };
        if self.is_negative {
            start -= 1;
            out[start] = b'-';
        }
        Ok(&out[start..])
    }

    /// BigInt::toString of ECMA-262.
    pub fn to_string_ref(self, radix: u32, poll: Poll<'_>) -> Result<StringRef, Error> {
        let length = self.max_text_length(radix);
        if radix.is_power_of_two() && length > 1 {
            let mut writer = StringWriter::try_new(length).map_err(|_| Error::OutOfMemory)?;
            self.write_text(radix, writer.push_repeated(b'0', length), poll)?;
            return Ok(writer.finish());
        }
        let mut scratch = Scratch::<u8, 1024>::new();
        match self.write_text(radix, scratch.filled(b'0', length)?, poll)? {
            &[character] => Ok(StringRef::single_character(character)),
            text => StringRef::try_from_units(text).map_err(|_| Error::OutOfMemory),
        }
    }
}

/// Returns where the text starts.
fn write_bit_groups(value: &[u64], bits: u32, out: &mut [u8]) -> usize {
    let start = out.len() - digits::bit_length(value).div_ceil(bits as usize);
    for (index, out) in out[start..].iter_mut().rev().enumerate() {
        let at = index * bits as usize;
        let group =
            digits::digit_or_zero(value, at / 64 + 1).funnel_shr(value[at / 64], (at % 64) as u32);
        *out = CHARACTERS[(group & ((1 << bits) - 1)) as usize];
    }
    start
}

/// Returns where the text starts.
fn write_by_division(
    value: &[u64],
    radix: u32,
    out: &mut [u8],
    work: &mut Work<'_>,
) -> Result<usize, Error> {
    let chunk = CHUNKS[radix as usize];
    let radix = u64::from(radix);
    let mut scratch = Scratch::<u64, 64>::new();
    let mut rest = scratch.filled(0, value.len())?;
    rest.copy_from_slice(value);
    let mut start = out.len();
    while rest.len() > 1 {
        work.spend(rest.len())?;
        let mut part = digits::divide_assign_by_digit(rest, chunk.power);
        for out in out[start - chunk.characters..start].iter_mut().rev() {
            *out = CHARACTERS[(part % radix) as usize];
            part /= radix;
        }
        start -= chunk.characters;
        let length = digits::trim(rest).len();
        rest = &mut rest[..length];
    }
    let mut part = rest[0];
    while part != 0 {
        start -= 1;
        out[start] = CHARACTERS[(part % radix) as usize];
        part /= radix;
    }
    Ok(start)
}
