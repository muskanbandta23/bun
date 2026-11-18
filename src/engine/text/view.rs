use core::cmp::Ordering;
use core::ops::Range;

use crate::{CodeUnit, hash_latin1, hash_utf16};

/// Borrowed characters of either width.
#[derive(Clone, Copy)]
pub enum Text<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

/// Runs an expression over the units of a [`Text`] as a slice, once for each width.
#[macro_export]
macro_rules! with_text {
    ($text:expr, |$units:ident| $body:expr) => {
        match $text {
            $crate::Text::Latin1($units) => $body,
            $crate::Text::Utf16($units) => $body,
        }
    };
}

impl<'a> Text<'a> {
    pub const EMPTY: Text<'static> = Text::Latin1(&[]);

    /// `ascii` has no byte above 0x7f.
    #[inline]
    pub const fn from_ascii(ascii: &'a str) -> Text<'a> {
        debug_assert!(ascii.is_ascii());
        Text::Latin1(ascii.as_bytes())
    }

    #[inline]
    pub const fn len(self) -> usize {
        with_text!(self, |units| units.len())
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub const fn is_latin1(self) -> bool {
        matches!(self, Text::Latin1(_))
    }

    /// Panics past the end.
    #[inline]
    pub fn at(self, index: usize) -> u16 {
        with_text!(self, |units| units[index].to_u16())
    }

    #[inline]
    pub fn get(self, index: usize) -> Option<u16> {
        with_text!(self, |units| units.get(index).map(|unit| unit.to_u16()))
    }

    /// CodePointAt of ECMA-262: a lone surrogate is itself. Panics past the end.
    #[inline]
    pub fn code_point_at(self, index: usize) -> u32 {
        match self {
            Text::Latin1(characters) => u32::from(characters[index]),
            Text::Utf16(units) => code_point_at(units, index),
        }
    }

    /// Panics if the range is not inside the text.
    #[inline]
    pub fn slice(self, range: Range<usize>) -> Text<'a> {
        with_text!(self, |units| CodeUnit::text(&units[range]))
    }

    #[inline]
    pub fn slice_from(self, start: usize) -> Text<'a> {
        with_text!(self, |units| CodeUnit::text(&units[start..]))
    }

    #[inline]
    pub fn code_units(self) -> CodeUnits<'a> {
        CodeUnits {
            text: self,
            range: 0..self.len(),
        }
    }

    #[inline]
    pub fn code_points(self) -> CodePoints<'a> {
        CodePoints { text: self, at: 0 }
    }

    #[inline]
    pub fn hash(self) -> u32 {
        match self {
            Text::Latin1(characters) => hash_latin1(characters),
            Text::Utf16(units) => hash_utf16(units),
        }
    }

    pub fn is_ascii(self) -> bool {
        match self {
            Text::Latin1(characters) => characters.is_ascii(),
            Text::Utf16(units) => units.iter().fold(0, |all, &unit| all | unit) < 0x80,
        }
    }

    /// Whether every unit fits in a byte, whichever width stores it.
    pub fn fits_latin1(self) -> bool {
        match self {
            Text::Latin1(_) => true,
            Text::Utf16(units) => units.iter().fold(0, |all, &unit| all | unit) < 0x100,
        }
    }

    #[inline]
    pub fn eq_ascii(self, ascii: &str) -> bool {
        self == Text::from_ascii(ascii)
    }

    pub fn eq_ignore_ascii_case(self, other: Text<'_>) -> bool {
        let fold = |unit: u16| {
            if unit.is_ascii_alphabetic() {
                unit | 0x20
            } else {
                unit
            }
        };
        self.len() == other.len()
            && self
                .code_units()
                .zip(other.code_units())
                .all(|(a, b)| fold(a) == fold(b))
    }

    #[inline]
    pub fn starts_with(self, prefix: Text<'_>) -> bool {
        self.len() >= prefix.len() && self.slice(0..prefix.len()) == prefix
    }

    #[inline]
    pub fn ends_with(self, suffix: Text<'_>) -> bool {
        self.len() >= suffix.len() && self.slice_from(self.len() - suffix.len()) == suffix
    }
}

#[inline]
fn code_point_at(units: &[u16], index: usize) -> u32 {
    let first = units[index];
    if first & 0xfc00 == 0xd800
        && let Some(&second) = units.get(index + 1)
        && second & 0xfc00 == 0xdc00
    {
        return 0x10000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
    }
    u32::from(first)
}

impl PartialEq<Text<'_>> for Text<'_> {
    #[inline]
    fn eq(&self, other: &Text<'_>) -> bool {
        match (*self, *other) {
            (Text::Latin1(a), Text::Latin1(b)) => a == b,
            (Text::Utf16(a), Text::Utf16(b)) => a == b,
            (Text::Latin1(a), Text::Utf16(b)) | (Text::Utf16(b), Text::Latin1(a)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(&a, &b)| u16::from(a) == b)
            }
        }
    }
}

impl Eq for Text<'_> {}

impl PartialOrd for Text<'_> {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// By code unit, as IsLessThan of ECMA-262 compares strings.
impl Ord for Text<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Text::Latin1(a), Text::Latin1(b)) => a.cmp(b),
            (Text::Utf16(a), Text::Utf16(b)) => a.cmp(b),
            _ => self.code_units().cmp(other.code_units()),
        }
    }
}

impl core::fmt::Display for Text<'_> {
    /// A lone surrogate is written as U+FFFD.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use core::fmt::Write;
        self.code_points().try_for_each(|code_point| {
            formatter.write_char(char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER))
        })
    }
}

impl core::fmt::Debug for Text<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "\"{self}\"")
    }
}

#[derive(Clone)]
pub struct CodeUnits<'a> {
    text: Text<'a>,
    range: Range<usize>,
}

impl Iterator for CodeUnits<'_> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<u16> {
        self.range.next().map(|index| self.text.at(index))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl DoubleEndedIterator for CodeUnits<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<u16> {
        self.range.next_back().map(|index| self.text.at(index))
    }
}

impl ExactSizeIterator for CodeUnits<'_> {}

#[derive(Clone)]
pub struct CodePoints<'a> {
    text: Text<'a>,
    at: usize,
}

impl Iterator for CodePoints<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        if self.at >= self.text.len() {
            return None;
        }
        let code_point = self.text.code_point_at(self.at);
        self.at += if code_point > 0xffff { 2 } else { 1 };
        Some(code_point)
    }
}
