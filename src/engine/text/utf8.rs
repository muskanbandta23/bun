//! From UTF-8. What is written stays inside its room even if the bytes change between two looks at them.

use core::hint::select_unpredictable;
use core::mem::MaybeUninit;

use bun_alloc::AllocError;
use bun_simdutf_sys::simdutf::{self, WithUtf16Len};

use crate::{CodeUnit, Cursor, StringBuilder, StringRef, Text, ascii};

/// Up to here the vector code does not make up for the call.
const SHORT: usize = 12;
/// What is converted on the stack, where there is room for the worst case, and then copied.
const SCRATCH: usize = 1024;
/// The highest first byte of a character up to U+00FF.
const LAST_LATIN1_LEAD: u8 = 0xc3;
const REPLACEMENT: u32 = 0xfffd;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Invalid {
    Stops,
    IsReplaced,
}

#[inline(always)]
fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// For each byte the lowest that can follow it in a sequence, and how many can.
static SECOND_BYTES: [[u8; 2]; 256] = {
    let (mut ranges, mut first) = ([[0; 2]; 256], 0xc2);
    while first <= 0xf4 {
        ranges[first] = match first {
            0xe0 => [0xa0, 0x20],
            0xed => [0x80, 0x20],
            0xf0 => [0x90, 0x30],
            0xf4 => [0x80, 0x10],
            _ => [0x80, 0x40],
        };
        first += 1;
    }
    ranges
};

/// Whether the first two of `bytes`, which are not empty, belong together.
#[inline(always)]
fn starts_with_sequence(bytes: &[u8]) -> bool {
    let [lowest, count] = SECOND_BYTES[usize::from(bytes[0])];
    bytes.get(1).map_or(0, |&it| it).wrapping_sub(lowest) < count
}

/// The character that `bytes` start with and its length, if their first two belong together. The error is the length
/// of a sequence that ends early.
#[inline(always)]
fn decode_sequence(bytes: &[u8]) -> Result<(u32, usize), usize> {
    let bits = |byte: u8, mask: u8, shift: u32| u32::from(byte & mask) << shift;
    match *bytes {
        [a @ ..0xe0, b, ..] => Ok((bits(a, 0x1f, 6) | bits(b, 0x3f, 0), 2)),
        [a @ ..0xf0, b, c, ..] if is_continuation(c) => {
            Ok((bits(a, 0x0f, 12) | bits(b, 0x3f, 6) | bits(c, 0x3f, 0), 3))
        }
        [a, b, c, d, ..] if is_continuation(c) & is_continuation(d) => Ok((
            bits(a, 0x07, 18) | bits(b, 0x3f, 12) | bits(c, 0x3f, 6) | bits(d, 0x3f, 0),
            4,
        )),
        [_, _, c, ..] if is_continuation(c) => Err(3),
        _ => Err(2),
    }
}

/// The character above U+007F that `bytes` start with, and its length.
#[inline(always)]
pub(crate) fn decode_non_ascii(bytes: &[u8; 4]) -> Option<(u32, usize)> {
    starts_with_sequence(bytes).then(|| decode_sequence(bytes).ok())?
}

/// Goes on up to the character that starts at `end` or later. Returns how many bytes it has read. There is room for
/// a unit for each of them.
#[inline(always)]
fn decode_characters(
    bytes: &[u8],
    end: usize,
    invalid: Invalid,
    cursor: &mut Cursor<'_, u16>,
) -> usize {
    assert!(end <= bytes.len());
    let mut read = 0;
    while read < end {
        let rest = &bytes[read..];
        let (code_point, length) = if starts_with_sequence(rest) {
            match decode_sequence(rest) {
                Ok(character) => character,
                Err(_) if invalid == Invalid::Stops => break,
                Err(length) => (REPLACEMENT, length),
            }
        } else if invalid == Invalid::Stops && !rest[0].is_ascii() {
            break;
        } else {
            let is_ascii = rest[0].is_ascii();
            (
                select_unpredictable(is_ascii, u32::from(rest[0]), REPLACEMENT),
                1,
            )
        };
        cursor.push_code_point(code_point);
        read += length;
    }
    read
}

#[inline(always)]
fn convert_valid(bytes: &[u8], cursor: &mut Cursor<'_, u16>) -> simdutf::SIMDUTFResult {
    let input = WithUtf16Len::utf8_worst_case(bytes, cursor.remaining()).expect("room");
    cursor.lend(|it| simdutf::convert::utf8::to::utf16::with_errors::le(input, it))
}

/// Returns how many bytes it has read. There is room for a unit for each byte.
fn decode_all(bytes: &[u8], invalid: Invalid, cursor: &mut Cursor<'_, u16>) -> usize {
    if bytes.len() <= SHORT || cfg!(miri) {
        return decode_characters(bytes, bytes.len(), invalid, cursor);
    }
    let result = convert_valid(bytes, cursor);
    if result.is_successful() {
        return bytes.len();
    }
    if invalid == Invalid::Stops {
        return 0;
    }
    decode_replacing(bytes, result.count, cursor);
    bytes.len()
}

/// The first `valid` bytes are valid. The closer together the invalid sequences are, the longer the stretches that
/// are gone through by character.
#[cold]
fn decode_replacing(mut bytes: &[u8], mut valid: usize, cursor: &mut Cursor<'_, u16>) {
    const SHORTEST_STRETCH: usize = 32;
    let mut stretch = SHORTEST_STRETCH;
    loop {
        if valid > stretch && convert_valid(&bytes[..valid], cursor).is_successful() {
            bytes = &bytes[valid..];
            stretch = SHORTEST_STRETCH;
        } else {
            stretch = (2 * stretch).min(SCRATCH);
        }
        let end = stretch.min(bytes.len());
        bytes = &bytes[decode_characters(bytes, end, Invalid::IsReplaced, cursor)..];
        if bytes.is_empty() {
            return;
        }
        valid = simdutf::validate::with_errors::utf8(bytes).count;
    }
}

/// The start of a sequence at or a little before `at`, so that what is before it decodes alike without the rest.
fn sequence_start(bytes: &[u8], at: usize) -> usize {
    (at.saturating_sub(3)..=at)
        .rfind(|&start| bytes.get(start).is_none_or(|&byte| !is_continuation(byte)))
        .unwrap_or(at)
}

/// Lends the units of up to `SCRATCH` bytes. `None` if they are invalid and that stops it.
#[inline(always)]
fn with_decoded<R>(bytes: &[u8], invalid: Invalid, take: impl FnOnce(&[u16]) -> R) -> Option<R> {
    let mut scratch = [MaybeUninit::uninit(); SCRATCH];
    let mut read = 0;
    let units = Cursor::fill_uninit(&mut scratch, |it| read = decode_all(bytes, invalid, it));
    (read == bytes.len()).then(|| take(units))
}

impl StringBuilder {
    /// Each maximal invalid sequence becomes U+FFFD, as in the WHATWG decoder.
    #[inline]
    pub fn push_utf8_lossy(&mut self, bytes: &[u8]) {
        if ascii::is_ascii(bytes) {
            self.push_latin1_slice(bytes);
        } else {
            self.push_non_ascii_utf8(bytes, Invalid::IsReplaced);
        }
    }

    /// Returns false if the bytes are invalid and that stops it.
    fn push_non_ascii_utf8(&mut self, mut bytes: &[u8], invalid: Invalid) -> bool {
        if bytes.len() > SCRATCH && !cfg!(miri) {
            if self.is_latin1() && ascii::find_above(bytes, LAST_LATIN1_LEAD).is_none() {
                self.grow(simdutf::length::latin1::from::utf8(bytes));
                bytes = self.push_in_spare::<u8>(bytes, |piece, cursor| {
                    cursor.lend(|it| simdutf::convert::utf8::to::latin1(piece, it))
                });
            } else {
                self.widen(simdutf::length::utf16::from::utf8(bytes));
                bytes = self.push_in_spare::<u16>(bytes, |piece, cursor| {
                    decode_all(piece, invalid, cursor) == piece.len()
                });
            }
        }
        while !bytes.is_empty() {
            let (piece, rest) = bytes.split_at(sequence_start(bytes, bytes.len().min(SCRATCH)));
            if with_decoded(piece, invalid, |units| self.push_utf16(units)).is_none() {
                return false;
            }
            bytes = rest;
        }
        true
    }

    /// Converts long pieces that fit in the worst case until `convert` fails. Returns what is left.
    #[inline(always)]
    fn push_in_spare<'a, T: CodeUnit>(
        &mut self,
        mut bytes: &'a [u8],
        convert: impl Fn(&[u8], &mut Cursor<'_, T>) -> bool,
    ) -> &'a [u8] {
        self.write_in_spare::<T>(|cursor| {
            while let fitting @ SCRATCH.. = bytes.len().min(cursor.remaining()) {
                let (piece, rest) = bytes.split_at(sequence_start(bytes, fitting));
                if !convert(piece, cursor) {
                    break;
                }
                bytes = rest;
            }
        });
        bytes
    }
}

#[inline(always)]
fn from_utf8(bytes: &[u8], invalid: Invalid) -> Result<Option<StringRef>, AllocError> {
    if ascii::is_ascii(bytes) {
        return StringRef::try_copy(Text::Latin1(bytes)).map(Some);
    }
    from_non_ascii_utf8(bytes, invalid)
}

/// The string of bytes that are UTF-8 with no character above U+00FF, which the first byte above
/// [`LAST_LATIN1_LEAD`] rules out. `None` if they are not.
fn from_latin1_utf8(bytes: &[u8]) -> Option<Result<StringRef, AllocError>> {
    let mut scratch = [MaybeUninit::uninit(); SCRATCH];
    let mut is_valid = false;
    let characters = Cursor::fill_uninit(&mut scratch, |cursor| {
        is_valid = if bytes.len() <= SHORT || cfg!(miri) {
            decode_latin1(bytes, cursor)
        } else {
            cursor.lend(|it| simdutf::convert::utf8::to::latin1(bytes, it))
        };
    });
    is_valid.then(|| StringRef::try_copy(Text::Latin1(characters)))
}

/// Returns whether the bytes are UTF-8 with no character above U+00FF. There is room for a byte of each.
#[inline(always)]
fn decode_latin1(bytes: &[u8], cursor: &mut Cursor<'_, u8>) -> bool {
    let mut read = 0;
    while read < bytes.len() {
        let rest = &bytes[read..];
        if rest[0].is_ascii() {
            cursor.push(rest[0]);
            read += 1;
        } else if starts_with_sequence(rest)
            && let Ok((character, length)) = decode_sequence(rest)
        {
            cursor.push(character as u8);
            read += length;
        } else {
            return false;
        }
    }
    true
}

fn from_non_ascii_utf8(bytes: &[u8], invalid: Invalid) -> Result<Option<StringRef>, AllocError> {
    if bytes.len() <= SCRATCH {
        if ascii::find_above(bytes, LAST_LATIN1_LEAD).is_none()
            && let Some(string) = from_latin1_utf8(bytes)
        {
            return string.map(Some);
        }
        let copy = |units: &[u16]| {
            let text = Text::Utf16(units);
            if text.fits_latin1() {
                StringRef::try_write::<u8>(text.len(), |cursor| cursor.push_text(text))
            } else {
                StringRef::try_write::<u16>(text.len(), |cursor| cursor.push_text(text))
            }
        };
        return with_decoded(bytes, invalid, copy).transpose();
    }
    let mut builder = StringBuilder::new();
    if !builder.push_non_ascii_utf8(bytes, invalid) {
        return Ok(None);
    }
    builder.finish().map(Some)
}

impl StringRef {
    /// `Ok(None)` if the bytes are not UTF-8. Latin-1 if the characters allow it.
    #[inline]
    pub fn try_from_utf8(bytes: &[u8]) -> Result<Option<StringRef>, AllocError> {
        from_utf8(bytes, Invalid::Stops)
    }

    /// Each maximal invalid sequence becomes U+FFFD, as in the WHATWG decoder. Latin-1 if the characters allow it.
    #[inline]
    pub fn try_from_utf8_lossy(bytes: &[u8]) -> Result<StringRef, AllocError> {
        from_utf8(bytes, Invalid::IsReplaced).map(Option::unwrap_or_default)
    }

    /// For text whose size no script picks.
    pub fn from_str(text: &str) -> StringRef {
        Self::try_from_utf8_lossy(text.as_bytes()).unwrap_or_else(|_| bun_alloc::out_of_memory())
    }
}
