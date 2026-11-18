//! The primitives that take more than a few instructions.

use bun_engine_number::{JsonNumber, parse_json_number};
use bun_engine_text::{CodeUnit, StringBuilder, Text, scan};

use super::reader::Cursor;
use super::strings::ShortText;
use super::{Parser, Tables};
use crate::json::records::Recorder;
use crate::json::syntax_error::Expected;
use crate::json::token::unescape;
use crate::json::{Source, with_units};
use crate::{JsString, Value};

const BYTES_ON_STACK: usize = 512;

/// What a string literal stands for.
enum Characters<'a> {
    Plain(Text<'a>),
    Decoded(&'a mut StringBuilder),
}

impl<'a, R: Recorder> Parser<'a, R> {
    #[inline(never)]
    pub(super) extern "rust-cold" fn any_primitive(
        &mut self,
        tables: &mut Tables,
        mut cursor: Cursor<'a>,
    ) -> (Cursor<'a>, Option<Value>) {
        let (value, length) = match self.reader.window(&mut cursor) {
            [b'"', ..] => return self.any_string(tables, cursor),
            [b'-' | b'0'..=b'9', ..] => return self.any_number(cursor),
            [b't', b'r', b'u', b'e', ..] => (Value::TRUE, 4),
            [b'f', b'a', b'l', b's', b'e', ..] => (Value::FALSE, 5),
            [b'n', b'u', b'l', b'l', ..] => (Value::NULL, 4),
            _ => {
                let nothing = self.unexpected(&cursor, Expected::Value);
                return (cursor, nothing);
            }
        };
        cursor.advance(length);
        (cursor, Some(value))
    }

    #[inline(never)]
    pub(super) extern "rust-cold" fn any_number(
        &mut self,
        mut cursor: Cursor<'a>,
    ) -> (Cursor<'a>, Option<Value>) {
        let position = self.position(&cursor);
        let number = with_units!(self.reader.source(), |units, _| {
            let rest = &units[position..];
            parse_json_number(rest).map(|(number, length)| {
                self.reader.skip(&mut cursor, rest, length);
                number
            })
        });
        let value = match number {
            Ok(JsonNumber::Int32(number)) => Some(number.into()),
            Ok(JsonNumber::Double(number)) => Some(number.into()),
            Err(_) => self.unexpected(&cursor, Expected::Value),
        };
        (cursor, value)
    }

    #[inline(never)]
    pub(super) extern "rust-cold" fn new_short_string(
        &mut self,
        tables: &mut Tables,
        text: ShortText,
    ) -> Option<Value> {
        let string = tables.strings.add_short(self.cx, text);
        self.or_out_of_memory(string).map(Into::into)
    }

    #[inline(never)]
    pub(super) extern "rust-cold" fn any_string(
        &mut self,
        tables: &mut Tables,
        mut cursor: Cursor<'a>,
    ) -> (Cursor<'a>, Option<Value>) {
        let cx = self.cx;
        let string = self.read_string(&mut cursor, |characters| match characters {
            Characters::Plain(text) => tables.strings.any(cx, text),
            Characters::Decoded(it) if it.len() <= BYTES_ON_STACK / 2 => {
                tables.strings.any(cx, it.text())
            }
            Characters::Decoded(it) => it.take().map(|string| JsString::new(cx, string)),
        });
        let value = string.and_then(|string| self.or_out_of_memory(string));
        (cursor, value.map(Into::into))
    }

    /// Reads the string literal at the cursor, and lends its characters.
    #[inline(always)]
    pub(super) fn with_characters<T>(
        &mut self,
        cursor: &mut Cursor<'a>,
        read: impl FnOnce(Text<'_>) -> T,
    ) -> Option<T> {
        self.read_string(cursor, |characters| match characters {
            Characters::Plain(text) => read(text),
            Characters::Decoded(it) => read(it.text()),
        })
    }

    #[inline(always)]
    fn read_string<T>(
        &mut self,
        cursor: &mut Cursor<'a>,
        read: impl FnOnce(Characters<'_>) -> T,
    ) -> Option<T> {
        let position = self.position(cursor);
        with_units!(self.reader.source(), |units, is_utf8| {
            let rest = &units[position..];
            let end = 1 + scan::json_string_run_len(&rest[1..]);
            let run = CodeUnit::text(&rest[1..end]);
            if rest.get(end).is_some_and(|unit| unit.is(b'"')) && (!is_utf8 || run.is_ascii()) {
                self.reader.skip(cursor, rest, end + 1);
                return Some(read(Characters::Plain(run)));
            }
            let Some((result, length)) = self.decode(rest, end, is_utf8, read) else {
                return self
                    .failure
                    .is_none()
                    .then(|| self.unexpected(cursor, Expected::Value))?;
            };
            self.reader.skip(cursor, rest, length);
            Some(result)
        })
    }

    /// `end` is that of the first run of units that are not special. Returns the length of the literal too.
    #[inline(never)]
    fn decode<T, U: CodeUnit>(
        &mut self,
        rest: &[U],
        mut end: usize,
        is_utf8: bool,
        read: impl FnOnce(Characters<'_>) -> T,
    ) -> Option<(T, usize)> {
        StringBuilder::on_stack::<BYTES_ON_STACK, _>(|characters| {
            let mut start = 1;
            loop {
                match U::text(&rest[start..end]) {
                    Text::Latin1(bytes) if is_utf8 => {
                        self.has_non_ascii_bytes |= !bytes.is_ascii();
                        characters.push_utf8_lossy(bytes);
                    }
                    text => characters.push_text(text),
                }
                let (unit, length) = match &rest[end..] {
                    [quote, ..] if quote.is(b'"') => break,
                    [backslash, escape @ ..] if backslash.is(b'\\') => unescape(escape).ok()?,
                    _ => return None,
                };
                characters.push_unit(unit);
                start = end + 1 + length;
                end = start + scan::json_string_run_len(&rest[start..]);
            }
            if characters.has_overflowed() {
                return self.out_of_memory();
            }
            Some((read(Characters::Decoded(characters)), end + 1))
        })
    }
}
