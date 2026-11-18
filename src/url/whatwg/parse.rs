//! From bytes to a record.

use bun_engine_text::{AllocError, MAX_LENGTH};

use crate::buffer::Buffer;
use crate::canonical::{self, Outcome};
use crate::components::Components;
use crate::input::{Source, Transcoder};
use crate::parser::{Parser, Tail};
use crate::record::{OwnedStorage, Record, Storage, View};
use crate::{Error, Result, Url};

/// Whether a label of an ASCII domain that starts with `xn--` has to be valid for UTS #46. The URL Standard does not
/// ask for it. The `URL` class does, as that of Node.js.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Punycode {
    Unchecked,
    Checked,
}

impl<'a> Parser<'a> {
    fn run(&mut self, base: Option<&View<'a>>) -> Result<()> {
        let tail = self.head(base)?;
        self.tail(tail)
    }

    #[inline]
    fn finish<S: OwnedStorage>(&self) -> Result<Record<S>> {
        Ok(Record {
            text: S::concat(&self.out.pieces(self.source.bytes.len()))?,
            parts: self.parts,
        })
    }
}

/// `share` makes the result if it is a part of `source`.
#[inline(always)]
pub(crate) fn parse<'a, S: OwnedStorage>(
    source: Source<'a>,
    base: Option<&Record<impl Storage>>,
    punycode: Punycode,
    share: impl FnOnce(&'a [u8]) -> core::result::Result<S, AllocError>,
) -> Result<Record<S>> {
    if source.bytes.len() > MAX_LENGTH {
        return Err(Error::OutOfMemory);
    }
    let authority = match canonical::recognize(source.bytes, punycode) {
        Outcome::Canonical(parts) => {
            let text = share(source.bytes)?;
            return Ok(Record { text, parts });
        }
        Outcome::LacksPath(parts) => {
            let text = S::concat(&[source.bytes, b"/"])?;
            return Ok(Record { text, parts });
        }
        Outcome::CanonicalAuthority(parts) => Some(parts),
        Outcome::Unknown => None,
    };
    parse_in_full(source, base, punycode, authority, share)
}

#[inline(never)]
fn parse_in_full<'a, S: OwnedStorage>(
    source: Source<'a>,
    base: Option<&Record<impl Storage>>,
    punycode: Punycode,
    authority: Option<Components>,
    share: impl FnOnce(&'a [u8]) -> core::result::Result<S, AllocError>,
) -> Result<Record<S>> {
    let base = base.and_then(Record::view);
    let base = base.as_ref();
    let source = source.trimmed();
    if !source.is_well_formed() {
        return parse_cleaned_up(source, base, punycode);
    }
    let mut parser = Parser::new(source, punycode, authority.unwrap_or(Components::INVALID));
    let outcome = match authority {
        Some(parts) => parser.tail(Tail::Segments(parts.path_start as usize + 1)),
        None => parser.run(base),
    };
    match outcome {
        Ok(()) if parser.out.is_input() => Ok(Record {
            text: share(source.bytes)?,
            parts: parser.parts,
        }),
        Ok(()) => parser.finish(),
        Err(Error::Invalid) if source.has_tab_or_newline() => {
            parse_cleaned_up(source, base, punycode)
        }
        Err(error) => Err(error),
    }
}

#[cold]
fn parse_cleaned_up<S: OwnedStorage>(
    source: Source<'_>,
    base: Option<&View<'_>>,
    punycode: Punycode,
) -> Result<Record<S>> {
    let (mut transcoder, mut storage) = (Transcoder::new(), Buffer::new());
    let source = transcoder
        .well_formed(source)?
        .without_tab_or_newline(&mut storage)?;
    let mut parser = Parser::new(source, punycode, Components::INVALID);
    parser.run(base)?;
    parser.finish()
}

/// `source` is well formed.
#[inline]
pub(crate) fn can_parse(source: Source<'_>, base: Option<&Url>, punycode: Punycode) -> bool {
    canonical::has_canonical_authority(source.bytes, punycode)
        || can_parse_in_full(source, base, punycode)
}

#[inline(never)]
fn can_parse_in_full(source: Source<'_>, base: Option<&Url>, punycode: Punycode) -> bool {
    let base = base.and_then(Record::view);
    let mut storage = Buffer::new();
    source
        .trimmed()
        .without_tab_or_newline(&mut storage)
        .is_ok_and(|source| {
            Parser::new(source, punycode, Components::INVALID)
                .head(base.as_ref())
                .is_ok()
        })
}
