//! To UTF-8. A lone surrogate becomes U+FFFD.

use bun_alloc::AllocError;
use bun_simdutf_sys::simdutf::{self, WithUtf8Len};

use crate::{CodeUnit, Cursor, Text, ascii, block, with_text};

/// Up to here the vector code does not make up for the call.
const SHORT: usize = 32;
const LONGEST_STRETCH: usize = 1024;

/// How far a conversion into limited room has come.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Progress {
    pub read: usize,
    pub written: usize,
}

/// The bytes of the first character of `units`, which are not empty, in the first `.1` of `.0`, and the number of
/// its units.
#[inline(always)]
pub(crate) fn encode<T: CodeUnit>(units: &[T]) -> ([u8; 4], usize, usize) {
    let tail = |value: u32, shift: u32| 0x80 | (value >> shift & 0x3f) as u8;
    let unit = units[0].to_u32();
    match unit {
        ..0x80 => ([unit as u8, 0, 0, 0], 1, 1),
        0x80..0x800 => ([0xc0 | (unit >> 6) as u8, tail(unit, 0), 0, 0], 2, 1),
        0xd800..0xe000 => match T::text(units).code_point_at(0) {
            ..0x1_0000 => ([0xef, 0xbf, 0xbd, 0], 3, 1),
            pair => {
                let lead = 0xf0 | (pair >> 18) as u8;
                ([lead, tail(pair, 12), tail(pair, 6), tail(pair, 0)], 4, 2)
            }
        },
        _ => {
            let lead = 0xe0 | (unit >> 12) as u8;
            ([lead, tail(unit, 6), tail(unit, 0), 0], 3, 1)
        }
    }
}

/// Goes on up to the character that starts at `end` or later, or that does not fit. Returns how many units it has
/// read.
#[inline(always)]
fn encode_characters<T: CodeUnit>(units: &[T], end: usize, cursor: &mut Cursor<'_, u8>) -> usize {
    let mut read = 0;
    while read < end {
        let ([a, b, c, d], length, width) = encode(&units[read..]);
        if length > cursor.remaining() {
            break;
        }
        match length {
            1 => cursor.push(a),
            2 => cursor.push_array([a, b]),
            3 => cursor.push_array([a, b, c]),
            _ => cursor.push_array([a, b, c, d]),
        }
        read += width;
    }
    read
}

/// There is room for all of it.
fn encode_utf16(input: WithUtf8Len<'_, u16>, cursor: &mut Cursor<'_, u8>) {
    let result = cursor.lend(|it| simdutf::convert::utf16::to::utf8::with_errors::le(input, it));
    if !result.is_successful() {
        encode_replacing(input.slice(), result.count, cursor);
    }
}

/// The first `valid` units are well formed. The closer together the lone surrogates are, the longer the stretches
/// that are gone through by character.
#[cold]
fn encode_replacing(mut units: &[u16], mut valid: usize, cursor: &mut Cursor<'_, u8>) {
    let mut stretch = SHORT;
    loop {
        if valid > stretch {
            let (piece, rest) = units.split_at(valid);
            encode_utf16(WithUtf8Len::utf16(piece), cursor);
            units = rest;
            stretch = SHORT;
        } else {
            stretch = (2 * stretch).min(LONGEST_STRETCH);
        }
        units = &units[encode_characters(units, stretch.min(units.len()), cursor)..];
        if units.is_empty() {
            return;
        }
        valid = simdutf::validate::with_errors::utf16le(units).count;
    }
}

/// Returns how many units it has read.
fn encode_fitting<T: CodeUnit>(units: &[T], cursor: &mut Cursor<'_, u8>) -> usize {
    let mut rest = units;
    loop {
        let candidates = &rest[..rest.len().min(cursor.remaining())];
        let ascii = ascii::find_non_ascii(candidates).unwrap_or(candidates.len());
        cursor.push_text(T::text(&rest[..ascii]));
        rest = &rest[ascii..];

        let worst = if T::IS_LATIN1 { 2 } else { 3 };
        let mut fitting = rest.len().min(cursor.remaining() / worst);
        if fitting <= SHORT || cfg!(miri) {
            return units.len() - rest.len() + encode_characters(rest, rest.len(), cursor);
        }
        fitting -= usize::from(fitting < rest.len() && rest[fitting - 1].is_lead_surrogate());
        match T::text(&rest[..fitting]) {
            Text::Latin1(piece) => {
                let input = WithUtf8Len::latin1_worst_case(piece);
                cursor.lend(|it| simdutf::convert::latin1::to::utf8(input, it));
            }
            Text::Utf16(piece) => {
                let input = WithUtf8Len::utf16_worst_case(piece, cursor.remaining());
                encode_utf16(input.expect("room"), cursor);
            }
        }
        rest = &rest[fitting..];
    }
}

fn short_utf8_len<T: CodeUnit>(units: &[T]) -> usize {
    let count =
        |test: fn(u16) -> bool| block::count(block::Units(units, |it: T| test(it.to_u16())));
    let ascii = count(|unit| unit < 0x80);
    if T::IS_LATIN1 || ascii == units.len() {
        return 2 * units.len() - ascii;
    }
    let pairs = block::Pairs::new(units, &units[1..], |a: T, b: T| {
        a.is_lead_surrogate() & b.is_trail_surrogate()
    });
    3 * units.len() - ascii - count(|unit| unit < 0x800) - 2 * block::count(pairs)
}

impl Text<'_> {
    pub fn utf8_len(self) -> usize {
        match self {
            _ if self.len() <= SHORT || cfg!(miri) => with_text!(self, |it| short_utf8_len(it)),
            Text::Latin1(characters) => simdutf::length::utf8::from::latin1(characters),
            Text::Utf16(units) => simdutf::length::utf8::from::utf16::le_with_replacement(units),
        }
    }

    /// The characters themselves, if they are stored as UTF-8 would have them.
    #[inline]
    pub fn as_utf8(&self) -> Option<&[u8]> {
        u8::units(*self).filter(|characters| ascii::is_ascii(characters))
    }

    pub fn try_to_utf8(self) -> Result<Vec<u8>, AllocError> {
        let mut bytes = Vec::new();
        match self {
            _ if self.len() <= SHORT || cfg!(miri) => {
                Cursor::extend_vec(&mut bytes, self.utf8_len(), |cursor| {
                    self.write_utf8(cursor);
                })?;
            }
            Text::Latin1(characters) => {
                let input = WithUtf8Len::latin1(characters);
                Cursor::extend_vec(&mut bytes, input.utf8_len(), |cursor| {
                    if input.utf8_len() == characters.len() {
                        cursor.push_slice(characters);
                    } else {
                        cursor.lend(|it| simdutf::convert::latin1::to::utf8(input, it));
                    }
                })?;
            }
            Text::Utf16(units) => {
                let input = WithUtf8Len::utf16_with_replacement(units);
                Cursor::extend_vec(&mut bytes, input.utf8_len(), |cursor| {
                    encode_utf16(input, cursor);
                })?;
            }
        }
        Ok(bytes)
    }

    /// As many whole characters as fit. Returns how many units it has read.
    pub fn write_utf8(self, cursor: &mut Cursor<'_, u8>) -> usize {
        with_text!(self, |units| encode_fitting(units, cursor))
    }

    /// `encodeInto` of the Encoding Standard: as many whole characters as fit, and the rest of `target` as it was.
    pub fn encode_utf8_into(self, target: &mut [u8]) -> Progress {
        let mut read = 0;
        let written = Cursor::fill(target, |cursor| read = self.write_utf8(cursor));
        Progress { read, written }
    }
}
