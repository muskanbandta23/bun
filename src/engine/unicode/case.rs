//! Default Case Conversion of the Unicode Standard, section 3.13.

use core::ops::RangeInclusive;

use bun_engine_text::{AllocError, CodeUnit, StringRef, StringWriter, Text};

use crate::tables::{
    CASE_BLOCK_INDEX, CASE_BLOCKS, CASE_IGNORABLE, CASED, LOWER_CASE_DELTAS, UPPER_CASE_DELTAS,
    UPPER_CASE_EXPANSIONS,
};

/// In place of a delta: the result is several characters or depends on the context.
pub(crate) const SPECIAL: i32 = i32::MIN;

/// What to add to `c` to map it.
#[inline(always)]
pub(crate) fn delta(c: u32, deltas: &[i32; 256]) -> i32 {
    match CASE_BLOCK_INDEX.get((c >> 6) as usize) {
        Some(&block) => deltas[usize::from(CASE_BLOCKS[usize::from(block)][(c & 63) as usize])],
        None => 0,
    }
}

/// `None` if nothing changes.
pub fn to_lower_case(text: Text<'_>) -> Result<Option<StringRef>, AllocError> {
    match text {
        Text::Latin1(characters) => {
            let Some(unchanged) = position(characters, |c| lower_latin1(c) != c) else {
                return Ok(None);
            };
            let mut out = StringWriter::try_new(characters.len())?;
            out.push_slice(&characters[..unchanged]);
            out.push_mapped(&characters[unchanged..], lower_latin1);
            Ok(Some(out.finish()))
        }
        Text::Utf16(units) => convert_utf16::<Lower>(units),
    }
}

/// `None` if nothing changes.
pub fn to_upper_case(text: Text<'_>) -> Result<Option<StringRef>, AllocError> {
    match text {
        Text::Latin1(characters) => {
            let changes = |c| (upper_latin1(c) != c) | is_irregular_in_upper_case(c);
            let Some(unchanged) = position(characters, changes) else {
                return Ok(None);
            };
            if position(&characters[unchanged..], is_irregular_in_upper_case).is_some() {
                return irregular_latin1_to_upper_case(characters).map(Some);
            }
            let mut out = StringWriter::try_new(characters.len())?;
            out.push_slice(&characters[..unchanged]);
            out.push_mapped(&characters[unchanged..], upper_latin1);
            Ok(Some(out.finish()))
        }
        Text::Utf16(units) => convert_utf16::<Upper>(units),
    }
}

const CHUNK: usize = 16;

/// A position before which nothing passes `test`, less than a chunk before the first unit that does.
#[inline(always)]
fn position<T: Copy>(units: &[T], test: impl Fn(T) -> bool) -> Option<usize> {
    let any = |chunk: &[T; CHUNK]| chunk.iter().fold(false, |any, &unit| any | test(unit));
    let Some(last) = units.last_chunk() else {
        return units.iter().position(|&unit| test(unit));
    };
    match units.as_chunks().0.iter().position(any) {
        Some(index) => Some(index * CHUNK),
        None => any(last).then(|| units.len() - CHUNK),
    }
}

#[inline(always)]
fn within<T: CodeUnit>(unit: T, range: RangeInclusive<u16>) -> bool {
    let (first, last) = range.into_inner();
    T::from_u16_truncating(unit.to_u16().wrapping_sub(first))
        <= T::from_u16_truncating(last - first)
}

#[inline(always)]
fn lower_latin1(c: u8) -> u8 {
    let is_upper = within(c, 0x41..=0x5a) | (within(c, 0xc0..=0xde) & (c != 0xd7));
    c | u8::from(is_upper) << 5
}

/// Leaves the irregular characters as they are.
#[inline(always)]
fn upper_latin1(c: u8) -> u8 {
    let is_lower = within(c, 0x61..=0x7a) | (within(c, 0xe0..=0xfe) & (c != 0xf7));
    c ^ u8::from(is_lower) << 5
}

const MICRO: u8 = 0xb5;
const SHARP_S: u8 = 0xdf;
const Y_WITH_DIAERESIS: u8 = 0xff;

#[inline(always)]
fn is_irregular_in_upper_case(c: u8) -> bool {
    (c == MICRO) | (c == SHARP_S) | (c == Y_WITH_DIAERESIS)
}

#[cold]
fn irregular_latin1_to_upper_case(characters: &[u8]) -> Result<StringRef, AllocError> {
    fn write<T: CodeUnit>(characters: &[u8]) -> Result<StringRef, AllocError> {
        let doubled = characters.iter().filter(|&&c| c == SHARP_S).count();
        let mut out = StringWriter::try_new(characters.len() + doubled)?;
        for &c in characters {
            match c {
                SHARP_S => out.push_slice(&[T::from(b'S'); 2]),
                MICRO => out.push(T::from_u16_truncating(0x39c)),
                Y_WITH_DIAERESIS => out.push(T::from_u16_truncating(0x178)),
                _ => out.push(T::from(upper_latin1(c))),
            }
        }
        Ok(out.finish())
    }
    match position(characters, |c| (c == MICRO) | (c == Y_WITH_DIAERESIS)) {
        Some(_) => write::<u16>(characters),
        None => write::<u8>(characters),
    }
}

trait Conversion {
    const DELTAS: &[i32; 256];
    /// The letters that change.
    const ASCII: RangeInclusive<u16>;

    /// The result for the special character `c` at `at`, padded with zeros.
    fn special(c: u32, units: &[u16], at: usize) -> [u16; 3];

    #[inline(always)]
    fn ascii(unit: u16) -> u16 {
        unit ^ u16::from(within(unit, Self::ASCII)) << 5
    }
}

struct Lower;

impl Conversion for Lower {
    const DELTAS: &[i32; 256] = &LOWER_CASE_DELTAS;
    const ASCII: RangeInclusive<u16> = 0x41..=0x5a;

    fn special(c: u32, units: &[u16], at: usize) -> [u16; 3] {
        match c {
            0x130 => [0x69, 0x307, 0],
            _ if is_final_sigma(units, at) => [0x3c2, 0, 0],
            _ => [0x3c3, 0, 0],
        }
    }
}

struct Upper;

impl Conversion for Upper {
    const DELTAS: &[i32; 256] = &UPPER_CASE_DELTAS;
    const ASCII: RangeInclusive<u16> = 0x61..=0x7a;

    fn special(c: u32, _: &[u16], _: usize) -> [u16; 3] {
        UPPER_CASE_EXPANSIONS[UPPER_CASE_EXPANSIONS.partition_point(|it| it.0 < c)].1
    }
}

/// Some of the units that neither conversion changes. A trail surrogate changes only behind a lead that is not among them.
#[inline(always)]
fn is_caseless(unit: u16) -> bool {
    within(unit, 0x0590..=0x109f)
        | within(unit, 0x2000..=0x2125)
        | within(unit, 0x2d30..=0xa63f)
        | within(unit, 0xabc0..=0xd800)
        | within(unit, 0xd83b..=0xfaff)
        | within(unit, 0xfb18..=0xff20)
}

#[inline(always)]
fn is_surrogate(unit: u16) -> bool {
    unit & 0xf800 == 0xd800
}

#[inline(always)]
fn has_only_ascii_and_caseless(chunk: &[u16; CHUNK]) -> bool {
    chunk.iter().fold(0, |all, &unit| all | unit) < 0x80
        || (chunk.iter()).fold(true, |all, &unit| all & ((unit < 0x80) | is_caseless(unit)))
}

/// The start of a character before which nothing changes. `None` if nothing does.
fn first_change<C: Conversion>(units: &[u16]) -> Option<usize> {
    let mut at = 0;
    while at < units.len() {
        if let Some(chunk) = units[at..].first_chunk()
            && has_only_ascii_and_caseless(chunk)
        {
            if (chunk.iter()).fold(false, |any, &unit| any | within(unit, C::ASCII)) {
                return Some(at);
            }
            at += CHUNK;
            continue;
        }
        let end = units.len().min(at + CHUNK);
        while at < end {
            let c = Text::Utf16(units).code_point_at(at);
            if delta(c, C::DELTAS) != 0 {
                return Some(at);
            }
            at += if c > 0xffff { 2 } else { 1 };
        }
    }
    None
}

fn convert_utf16<C: Conversion>(units: &[u16]) -> Result<Option<StringRef>, AllocError> {
    let Some(unchanged) = first_change::<C>(units) else {
        return Ok(None);
    };
    let mut length = units.len();
    loop {
        let mut out = StringWriter::try_new(length)?;
        out.push_slice(&units[..unchanged]);
        match write_converted::<C>(units, unchanged, &mut out) {
            Ok(()) => return Ok(Some(out.finish())),
            Err(at) => length = at + converted_length::<C>(units, at),
        }
    }
}

/// Fails with the position of the first character that leaves no room for the rest.
fn write_converted<C: Conversion>(
    units: &[u16],
    mut at: usize,
    out: &mut StringWriter<u16>,
) -> Result<(), usize> {
    while at < units.len() {
        if let Some(chunk) = units[at..].first_chunk() {
            if has_only_ascii_and_caseless(chunk) {
                out.push_mapped(chunk, C::ascii);
                at += CHUNK;
                continue;
            }
            let mut is_simple = true;
            let mapped = chunk.map(|unit| {
                let delta = delta(u32::from(unit), C::DELTAS);
                is_simple &= (delta != SPECIAL) & !is_surrogate(unit);
                unit.wrapping_add(delta as u16)
            });
            if is_simple {
                out.push_slice(&mapped);
                at += CHUNK;
                continue;
            }
        }
        let end = units.len().min(at + CHUNK);
        while at < end {
            let c = Text::Utf16(units).code_point_at(at);
            let width = if c > 0xffff { 2 } else { 1 };
            match delta(c, C::DELTAS) {
                SPECIAL => {
                    let special = C::special(c, units, at);
                    let special = trim(&special);
                    if out.remaining() < special.len() + (units.len() - at - width) {
                        return Err(at);
                    }
                    out.push_slice(special);
                }
                delta if width == 1 => out.push((c as u16).wrapping_add(delta as u16)),
                delta => {
                    let offset = c.wrapping_add_signed(delta) - 0x1_0000;
                    out.push(0xd800 | (offset >> 10) as u16);
                    out.push(0xdc00 | (offset & 0x3ff) as u16);
                }
            }
            at += width;
        }
    }
    Ok(())
}

fn trim(padded: &[u16; 3]) -> &[u16] {
    &padded[..1 + usize::from(padded[1] != 0) + usize::from(padded[2] != 0)]
}

#[cold]
fn converted_length<C: Conversion>(units: &[u16], mut at: usize) -> usize {
    let mut length = 0;
    while at < units.len() {
        let c = Text::Utf16(units).code_point_at(at);
        let width = if c > 0xffff { 2 } else { 1 };
        length += match delta(c, C::DELTAS) {
            SPECIAL => trim(&C::special(c, units, at)).len(),
            _ => width,
        };
        at += width;
    }
    length
}

/// Each of `ranges` is its first code point, then its size less one in the low 11 bits.
fn contains(ranges: &[u32], c: u32) -> bool {
    let candidates = &ranges[..ranges.partition_point(|&range| range >> 11 <= c)];
    (candidates.last()).is_some_and(|&range| c - (range >> 11) <= range & 0x7ff)
}

/// A character that is both cased and case-ignorable is ignored.
fn starts_with_cased(mut code_points: impl Iterator<Item = u32>) -> bool {
    (code_points.find(|&c| !contains(&CASE_IGNORABLE, c))).is_some_and(|c| contains(&CASED, c))
}

fn is_final_sigma(units: &[u16], at: usize) -> bool {
    let mut end = at;
    let before = core::iter::from_fn(|| {
        end = end.checked_sub(1)?;
        if end > 0 && Text::Utf16(units).code_point_at(end - 1) > 0xffff {
            end -= 1;
        }
        Some(Text::Utf16(units).code_point_at(end))
    });
    starts_with_cased(before) && !starts_with_cased(Text::Utf16(&units[at + 1..]).code_points())
}
