use core::mem::offset_of;

use bun_engine_text::{AllocError, StringImpl, StringRef, Text};

use crate::components::Components;
use crate::input::{Encoding, Input, Source, Transcoder};
use crate::record::{Record, concat};
use crate::{Error, Punycode, Result, parse, percent};

/// A URL, a text that has failed to parse as one, or nothing. Native code may read it in place:
///
/// | offset | type              | field          |                                                          |
/// | ------ | ----------------- | -------------- | -------------------------------------------------------- |
/// | 0      | `StringImpl*`     | text           | counted; null in a null URL; Latin-1 and ASCII if valid  |
/// | 8      | `u32`             | `scheme_end`   | at the colon                                             |
/// | 12     | `u32`             | `username_end` | the username starts at `scheme_end + 3` if that is less  |
/// | 16     | `u32`             | `host_start`   | from `username_end` up to here: nothing, `@`, `:secret@` |
/// | 20     | `u32`             | `host_end`     | from here up to `path_start`: nothing, `:port`, `/.`     |
/// | 24     | `u32`             | `path_start`   |                                                          |
/// | 28     | `u32`             | `path_end`     | at the `?` if less than `query_end`                      |
/// | 32     | `u32`             | `query_end`    | at the `#` if less than the length                       |
/// | 36     | `u16`             | port           | if there is a host and `path_start > host_end`           |
/// | 38     | `u8`              | scheme         | [`crate::Scheme`]                                        |
/// | 39     | `u8`              | state          | 0 invalid, 1 valid, 2 valid with an opaque path          |
///
/// There is a host, be it empty, if `host_start > scheme_end + 1`. All but the text is zero if the URL is invalid.
pub type Url = Record<Option<StringRef>>;

const _: () = {
    assert!(size_of::<Url>() == 40 && align_of::<Url>() == 8);
    assert!(offset_of!(Url, parts.scheme_end) == 8 && offset_of!(Url, parts.query_end) == 32);
    assert!(offset_of!(Url, parts.port) == 36 && offset_of!(Url, parts.state) == 39);
};

impl Url {
    pub const fn null() -> Url {
        Url {
            text: None,
            parts: Components::INVALID,
        }
    }

    pub(crate) fn invalid(text: StringRef) -> Url {
        Url {
            text: Some(text),
            parts: Components::INVALID,
        }
    }

    /// The basic URL parser. An invalid base is no base.
    #[inline]
    pub fn parse<'a>(
        input: impl Into<Input<'a>>,
        base: Option<&Url>,
        punycode: Punycode,
    ) -> Result<Url> {
        let (text, owner) = match input.into() {
            Input::String(string) => (string.text(), Some(string)),
            Input::Text(text) => (text, None),
            Input::Utf8(bytes) => {
                let source = Source::new(bytes, Encoding::UncheckedUtf8);
                return Url::parse_source(source, None, base, punycode);
            }
        };
        match text {
            Text::Latin1(bytes) => {
                Url::parse_source(Source::new(bytes, Encoding::Latin1), owner, base, punycode)
            }
            Text::Utf16(_) => Url::parse_utf16(text, base, punycode),
        }
    }

    fn parse_utf16(text: Text<'_>, base: Option<&Url>, punycode: Punycode) -> Result<Url> {
        Url::parse_source(Transcoder::new().source(text)?, None, base, punycode)
    }

    fn parse_source(
        source: Source<'_>,
        owner: Option<&StringImpl>,
        base: Option<&Url>,
        punycode: Punycode,
    ) -> Result<Url> {
        let share = |href: &[u8]| match owner {
            Some(owner) if owner.len() == href.len() => Ok(Some(StringRef::retain(owner))),
            _ => StringRef::try_from_units(href).map(Some),
        };
        parse::parse(source, base, punycode, share)
    }

    /// What does not parse becomes an invalid URL that has `input` for its text.
    pub fn parse_or_keep(
        input: &StringImpl,
        base: Option<&Url>,
    ) -> core::result::Result<Url, AllocError> {
        match Url::parse(input, base, Punycode::Unchecked) {
            Ok(url) => Ok(url),
            Err(Error::Invalid) => Ok(Url::invalid(StringRef::retain(input))),
            Err(Error::OutOfMemory) => Err(AllocError),
        }
    }

    /// Whether [`Self::parse`] finds `input` valid.
    pub fn can_parse(input: Text<'_>, base: Option<&Url>, punycode: Punycode) -> bool {
        Transcoder::new()
            .source(input)
            .is_ok_and(|source| parse::can_parse(source, base, punycode))
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.text.is_none()
    }

    /// Also if it is null.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.text.as_ref().is_none_or(|text| text.is_empty())
    }

    /// The serialization, or what has failed to parse.
    #[inline]
    pub fn string(&self) -> Option<&StringRef> {
        self.text.as_ref()
    }

    #[inline]
    pub fn into_string(self) -> Option<StringRef> {
        self.text
    }

    /// `None` if it is not UTF-8.
    pub fn decoded_username(&self) -> core::result::Result<Option<StringRef>, AllocError> {
        percent::decode_to_string(self.username())
    }

    /// `None` if it is not UTF-8.
    pub fn decoded_password(&self) -> core::result::Result<Option<StringRef>, AllocError> {
        percent::decode_to_string(self.password())
    }

    /// Without credentials, query and fragment. The text of an invalid URL is left alone.
    pub fn string_for_report(&self) -> core::result::Result<StringRef, AllocError> {
        if self.has_credentials() || (self.parts.path_end as usize) < self.href().len() {
            return concat(&self.without_credentials(self.parts.path_end));
        }
        Ok(self.text.clone().unwrap_or_default())
    }

    /// The text with `...` for its middle if it is too long to show.
    pub fn string_for_message(&self) -> StringRef {
        const LIMIT: usize = 1024;
        let string = self.text.clone().unwrap_or_default();
        if string.len() <= LIMIT {
            return string;
        }
        let text = string.text();
        let pieces = [
            text.slice(0..LIMIT / 2 - 1),
            Text::from_ascii("..."),
            text.slice_from(text.len() - (LIMIT / 2 - 2)),
        ];
        StringRef::try_concat(&pieces).unwrap_or_default()
    }
}

impl Default for Url {
    fn default() -> Url {
        Url::null()
    }
}

impl PartialEq for Url {
    fn eq(&self, other: &Url) -> bool {
        self.text == other.text
    }
}

impl Eq for Url {}

impl core::fmt::Debug for Url {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.text.fmt(formatter)
    }
}
