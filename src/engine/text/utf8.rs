use bun_alloc::AllocError;
use bun_simdutf_sys::simdutf;

use crate::{StringBuilder, StringRef, StringWriter, Text};

impl StringRef {
    /// `Ok(None)` if the bytes are not UTF-8.
    pub fn try_from_utf8(bytes: &[u8]) -> Result<Option<StringRef>, AllocError> {
        if simdutf::validate::ascii(bytes) {
            return StringRef::try_from_units(bytes).map(Some);
        }
        let Ok(text) = core::str::from_utf8(bytes) else {
            return Ok(None);
        };
        let mut writer = StringWriter::<u16>::try_new(simdutf::length::utf16::from::utf8(bytes))?;
        text.encode_utf16().for_each(|unit| writer.push(unit));
        Ok(Some(writer.finish()))
    }

    /// Each maximal invalid sequence becomes U+FFFD, as in the WHATWG decoder.
    pub fn try_from_utf8_lossy(bytes: &[u8]) -> Result<StringRef, AllocError> {
        if let Some(string) = Self::try_from_utf8(bytes)? {
            return Ok(string);
        }
        let mut builder = StringBuilder::with_capacity(bytes.len());
        for chunk in bytes.utf8_chunks() {
            chunk
                .valid()
                .encode_utf16()
                .for_each(|unit| builder.push_unit(unit));
            if !chunk.invalid().is_empty() {
                builder.push_unit(0xfffd);
            }
        }
        builder.finish()
    }

    /// For text whose size no script picks.
    pub fn from_str(text: &str) -> StringRef {
        Self::try_from_utf8_lossy(text.as_bytes()).unwrap_or_else(|_| bun_alloc::out_of_memory())
    }
}

impl Text<'_> {
    /// A lone surrogate becomes U+FFFD.
    pub fn try_to_utf8(self) -> Result<Vec<u8>, AllocError> {
        let length = match self {
            Text::Latin1(characters) => simdutf::length::utf8::from::latin1(characters),
            Text::Utf16(units) => simdutf::length::utf8::from::utf16::le_with_replacement(units),
        };
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| AllocError)?;
        match self {
            Text::Latin1(ascii) if ascii.len() == length => bytes.extend_from_slice(ascii),
            _ => {
                for code_point in self.code_points() {
                    let character =
                        char::from_u32(code_point).unwrap_or(char::REPLACEMENT_CHARACTER);
                    bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                }
            }
        }
        Ok(bytes)
    }

    /// IsStringWellFormedUnicode of ECMA-262.
    pub fn is_well_formed(self) -> bool {
        match self {
            Text::Latin1(_) => true,
            Text::Utf16(units) => simdutf::validate::utf16le(units),
        }
    }
}
