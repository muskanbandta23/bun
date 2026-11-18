use core::io::BorrowedBuf;
use core::mem::MaybeUninit;

use bun_engine_text::{AllocError, MAX_LENGTH, StringImpl, StringRef, Text};
use bun_simdutf_sys::simdutf::{self, WithUtf8Len};

use crate::buffer::Buffer;
use crate::class::{self, TAB_OR_NEWLINE};

/// What to parse.
#[derive(Clone, Copy)]
pub enum Input<'a> {
    /// A URL that serializes to the same Latin-1 characters shares the string.
    String(&'a StringImpl),
    Text(Text<'a>),
    /// What is not well formed stands for U+FFFD.
    Utf8(&'a [u8]),
}

impl<'a> From<&'a StringImpl> for Input<'a> {
    #[inline]
    fn from(string: &'a StringImpl) -> Input<'a> {
        Input::String(string)
    }
}

impl<'a> From<&'a StringRef> for Input<'a> {
    #[inline]
    fn from(string: &'a StringRef) -> Input<'a> {
        Input::String(string)
    }
}

impl<'a> From<Text<'a>> for Input<'a> {
    #[inline]
    fn from(text: Text<'a>) -> Input<'a> {
        Input::Text(text)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Latin1,
    Utf8,
    /// May not be well formed.
    UncheckedUtf8,
}

/// Text of a byte for each ASCII character, which is what the parser reads.
#[derive(Clone, Copy)]
pub(crate) struct Source<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) encoding: Encoding,
}

impl<'a> Source<'a> {
    #[inline]
    pub(crate) fn new(bytes: &'a [u8], encoding: Encoding) -> Source<'a> {
        Source { bytes, encoding }
    }

    #[inline]
    pub(crate) fn with_bytes<'b>(self, bytes: &'b [u8]) -> Source<'b> {
        Source::new(bytes, self.encoding)
    }

    #[inline]
    pub(crate) fn is_well_formed(self) -> bool {
        self.encoding != Encoding::UncheckedUtf8
            || self.bytes.is_ascii()
            || simdutf::validate::utf8(self.bytes)
    }

    /// Without leading and trailing C0 control or space.
    #[inline]
    pub(crate) fn trimmed(self) -> Source<'a> {
        let mut bytes = self.bytes;
        while let [0..=b' ', rest @ ..] | [rest @ .., 0..=b' '] = bytes {
            bytes = rest;
        }
        self.with_bytes(bytes)
    }

    pub(crate) fn has_tab_or_newline(self) -> bool {
        bun_highway::index_of_any_char(self.bytes, b"\t\n\r").is_some()
    }

    pub(crate) fn without_tab_or_newline<'b>(
        self,
        storage: &'b mut Buffer,
    ) -> Result<Source<'b>, AllocError>
    where
        'a: 'b,
    {
        if !self.has_tab_or_newline() {
            return Ok(self);
        }
        for &byte in self.bytes {
            if !class::is_in(TAB_OR_NEWLINE, byte) {
                storage.push(byte)?;
            }
        }
        Ok(self.with_bytes(storage))
    }
}

/// Holds what had to be converted to become a [`Source`].
pub(crate) struct Transcoder {
    inline: [MaybeUninit<u8>; Transcoder::INLINE_CAPACITY],
    heap: Vec<u8>,
}

impl Transcoder {
    const INLINE_CAPACITY: usize = 768;

    #[inline]
    pub(crate) fn new() -> Transcoder {
        Transcoder {
            inline: [const { MaybeUninit::uninit() }; Transcoder::INLINE_CAPACITY],
            heap: Vec::new(),
        }
    }

    #[inline]
    pub(crate) fn source<'a>(&'a mut self, text: Text<'a>) -> Result<Source<'a>, AllocError> {
        match text {
            Text::Latin1(characters) => Ok(Source::new(characters, Encoding::Latin1)),
            Text::Utf16(units) => Ok(Source::new(self.encode(units)?, Encoding::Utf8)),
        }
    }

    fn storage(&mut self, capacity: usize) -> Result<BorrowedBuf<'_, u8>, AllocError> {
        if capacity <= Transcoder::INLINE_CAPACITY {
            return Ok(BorrowedBuf::from(&mut self.inline[..]));
        }
        if capacity > MAX_LENGTH || self.heap.try_reserve_exact(capacity).is_err() {
            return Err(AllocError);
        }
        Ok(BorrowedBuf::from(self.heap.spare_capacity_mut()))
    }

    /// A lone surrogate becomes U+FFFD.
    fn encode(&mut self, units: &[u16]) -> Result<&[u8], AllocError> {
        let measured = WithUtf8Len::utf16_worst_case(units, Transcoder::INLINE_CAPACITY)
            .unwrap_or_else(|| WithUtf8Len::utf16_with_replacement(units));
        let mut out = self.storage(measured.utf8_len())?;
        if !simdutf::convert::utf16::to::utf8::with_errors::le(measured, out.unfilled())
            .is_successful()
        {
            for character in char::decode_utf16(units.iter().copied()) {
                let character = character.unwrap_or(char::REPLACEMENT_CHARACTER);
                out.unfilled()
                    .append(character.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Ok(out.into_filled())
    }

    /// Each maximal invalid sequence becomes U+FFFD.
    pub(crate) fn well_formed<'a>(
        &'a mut self,
        source: Source<'a>,
    ) -> Result<Source<'a>, AllocError> {
        const REPLACEMENT: &[u8] = "\u{fffd}".as_bytes();
        if source.is_well_formed() {
            return Ok(source);
        }
        let pieces = || {
            source.bytes.utf8_chunks().flat_map(|chunk| {
                let is_valid = chunk.invalid().is_empty();
                [
                    chunk.valid().as_bytes(),
                    if is_valid { b"" } else { REPLACEMENT },
                ]
            })
        };
        let mut out = self.storage(pieces().map(<[u8]>::len).sum())?;
        pieces().for_each(|piece| out.unfilled().append(piece));
        Ok(Source::new(out.into_filled(), Encoding::Utf8))
    }
}
