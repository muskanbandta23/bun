//! Strings that are long, have escapes or are not Latin-1.

use bun_engine_text::{CodeUnit, StringBuilder, Text, scan};

use super::{Parser, Tables};
use crate::json::records::Recorder;
use crate::json::syntax_error::Expected;
use crate::json::token::unescape;
use crate::json::{Encoding, Failure};
use crate::{JsString, Value};

impl<E: Encoding, R: Recorder> Parser<'_, E, R> {
    /// The value and the length of the literal that `rest` starts with.
    #[inline(never)]
    pub(super) fn any_string(&mut self, rest: &[E::Unit]) -> Result<(Value, usize), Failure> {
        let cx = self.cx;
        let end = 1 + scan::json_string_run_len(&rest[1..]);
        if let Some(text) = Self::plain(rest, end) {
            let string = self.tables.strings.any(cx, text);
            return Ok((cx.or_out_of_memory(string)?.into(), end + 1));
        }
        let (string, length) = self.with_decoded(rest, end, |tables, characters| {
            if characters.len() <= BYTES_ON_STACK / 2 {
                tables.strings.any(cx, characters.text())
            } else {
                characters.take().map(|string| JsString::new(cx, string))
            }
        })?;
        Ok((cx.or_out_of_memory(string)?.into(), length))
    }

    /// Lends the characters of the literal that `rest` starts with. Returns its length too.
    #[inline(always)]
    pub(super) fn with_characters<T>(
        &mut self,
        rest: &[E::Unit],
        read: impl FnOnce(&mut Tables, Text<'_>) -> T,
    ) -> Result<(T, usize), Failure> {
        let end = 1 + scan::json_string_run_len(&rest[1..]);
        match Self::plain(rest, end) {
            Some(text) => Ok((read(self.tables, text), end + 1)),
            None => self.with_decoded(rest, end, |tables, it| read(tables, it.text())),
        }
    }

    /// The characters, if the literal ends at `end` and they are the units before it.
    #[inline(always)]
    fn plain(rest: &[E::Unit], end: usize) -> Option<Text<'_>> {
        let run = E::Unit::text(&rest[1..end]);
        let is_closed = rest.get(end).is_some_and(|unit| unit.is(b'"'));
        (is_closed && (!E::IS_UTF8 || run.is_ascii())).then_some(run)
    }

    /// `end` is that of the first run of units that are not special.
    #[inline(never)]
    fn with_decoded<T>(
        &mut self,
        rest: &[E::Unit],
        mut end: usize,
        read: impl FnOnce(&mut Tables, &mut StringBuilder) -> T,
    ) -> Result<(T, usize), Failure> {
        StringBuilder::on_stack::<BYTES_ON_STACK, _>(|characters| {
            let mut start = 1;
            loop {
                self.decode(&rest[start..end], characters);
                let escape = match &rest[end..] {
                    [quote, ..] if quote.is(b'"') => break,
                    [backslash, escape @ ..] if backslash.is(b'\\') => unescape(escape).ok(),
                    _ => None,
                };
                let Some((unit, length)) = escape else {
                    return Err(self.unexpected(rest, Expected::Value));
                };
                characters.push_unit(unit);
                start = end + 1 + length;
                end = start + scan::json_string_run_len(&rest[start..]);
            }
            if characters.has_overflowed() {
                return Err(self.cx.throw_out_of_memory().into());
            }
            Ok((read(self.tables, characters), end + 1))
        })
    }

    #[inline(always)]
    fn decode(&mut self, run: &[E::Unit], characters: &mut StringBuilder) {
        match E::Unit::text(run) {
            Text::Latin1(bytes) if E::IS_UTF8 => {
                self.has_non_ascii_bytes |= !bytes.is_ascii();
                characters.push_utf8_lossy(bytes);
            }
            text => characters.push_text(text),
        }
    }
}

const BYTES_ON_STACK: usize = 512;
