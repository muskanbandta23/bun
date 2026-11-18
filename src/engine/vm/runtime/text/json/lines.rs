//! A value on each line, in a text that may break off anywhere.

use bun_engine_text::{CodeUnit, Text, scan};
use bun_simdutf_sys::simdutf;

use super::parser::Parser;
use super::{Encoding, Failure, Latin1, Source, Utf8, Utf16, with_source};
use crate::{Cx, JsResult, ValueVec};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum LinesEnd {
    Complete,
    NeedsMoreText,
    SyntaxError,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ParsedLines {
    /// The units up to the end of the last value. An invalid sequence of UTF-8 counts as the three bytes of U+FFFD.
    pub read: usize,
    pub end: LinesEnd,
}

/// Appends the values up to the first one that is incomplete or has an error.
pub fn parse_lines(cx: &Cx, source: Source<'_>, values: &mut ValueVec) -> JsResult<ParsedLines> {
    with_source!(source, parse_units(cx, _, source, values))
}

/// Whether the text breaks off between the digits before `at` and those of an exponent.
fn lacks_only_exponent<T: CodeUnit>(units: &[T], at: usize) -> bool {
    let (before, rest) = units.split_at(at);
    let letter = match rest {
        [letter] => letter,
        [letter, sign] if sign.is(b'+') || sign.is(b'-') => letter,
        _ => return false,
    };
    letter.to_ascii_lower().is(b'e') && before.last().is_some_and(|unit| unit.is_ascii_digit())
}

fn length_when_repaired(bytes: &[u8]) -> usize {
    if simdutf::validate::utf8(bytes) {
        return bytes.len();
    }
    let replacement = char::REPLACEMENT_CHARACTER.len_utf8();
    let length = |chunk: core::str::Utf8Chunk<'_>| {
        chunk.valid().len() + usize::from(!chunk.invalid().is_empty()) * replacement
    };
    bytes.utf8_chunks().map(length).sum()
}

fn parse_units<E: Encoding>(
    cx: &Cx,
    units: &[E::Unit],
    source: Source<'_>,
    values: &mut ValueVec,
) -> JsResult<ParsedLines> {
    Parser::<E, ()>::run(cx, units, (), |parser| {
        let mut read = 0;
        let end = loop {
            if parser.skip_white_space() == units.len() {
                break LinesEnd::Complete;
            }
            let value = match parser.value() {
                Ok(value) => value,
                Err(Failure::Thrown(thrown)) => return Err(thrown),
                Err(Failure::Syntax(error)) => {
                    let is_incomplete = lacks_only_exponent(units, error.at)
                        || cx.or_out_of_memory(error.is_at_end(source))?;
                    break if is_incomplete {
                        LinesEnd::NeedsMoreText
                    } else {
                        LinesEnd::SyntaxError
                    };
                }
            };
            let after = parser.position();
            if lacks_only_exponent(units, after) {
                break LinesEnd::NeedsMoreText;
            }
            cx.or_out_of_memory(values.try_push(value))?;
            read = after;
            let blank = |unit: u16| unit.is(b' ') | unit.is(b'\t') | unit.is(b'\r');
            let rest = &units[after..];
            let next = rest.get(scan::run_len(rest, blank));
            if next.is_some_and(|unit| !unit.is(b'\n')) {
                break LinesEnd::SyntaxError;
            }
        };
        if let Source::Utf8(bytes) = source
            && parser.has_non_ascii_bytes()
        {
            read = length_when_repaired(&bytes[..read]);
        }
        Ok(ParsedLines { read, end })
    })
}
