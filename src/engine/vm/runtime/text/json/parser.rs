use core::mem::MaybeUninit;

use bun_alloc::AllocError;
use bun_engine_number::{JsonNumber, parse_json_number};
use bun_engine_text::{
    CodeUnit, Cursor, Identifier, StringBuilder, StringRef, Text, parse_array_index, scan,
};

use super::records::Recorder;
use super::syntax_error::{Expected, SyntaxError};
use super::token::unescape;
use super::{Encoding, Failure};
use crate::{Attributes, Cx, JsResult, JsString, Value};

/// So many units can be looked at without asking where the text ends.
const WINDOW: usize = 64;

/// No collection runs while it is alive: all that it makes is part of the result.
pub(super) struct Parser<'a, E: Encoding, R> {
    cx: &'a Cx,
    /// Once the end is near, a copy that is followed by a window of zeros.
    rest: &'a [E::Unit],
    /// The position is this less the length of what is left.
    origin: usize,
    length: usize,
    tail: Option<&'a mut [MaybeUninit<E::Unit>; 2 * WINDOW]>,
    /// The members of the open objects and arrays, each run of them after the `Scope` to go back to.
    stack: Vec<Value>,
    keys: Vec<Key>,
    has_repaired_text: bool,
    pub(super) recorder: R,
}

enum Key {
    Name(Identifier),
    Index(u32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Outside,
    Object,
    Array,
}

#[derive(Clone, Copy)]
struct Scope {
    /// Where the members start on the stack.
    base: u32,
    kind: Kind,
}

impl Scope {
    #[inline(always)]
    fn to_value(self) -> Value {
        Value::double(f64::from_bits(u64::from(self.base) << 2 | self.kind as u64))
    }

    #[inline(always)]
    fn from_value(value: Value) -> Scope {
        let bits = value.double_unchecked().to_bits();
        let kind = match bits & 3 {
            0 => Kind::Outside,
            1 => Kind::Object,
            _ => Kind::Array,
        };
        Scope {
            base: (bits >> 2) as u32,
            kind,
        }
    }
}

#[inline(always)]
fn byte<T: CodeUnit>(unit: T) -> u8 {
    unit.to_u16().min(0xff) as u8
}

#[inline(always)]
fn starts_with<T: CodeUnit, const N: usize>(window: &[T; WINDOW], literal: &[u8; N]) -> bool {
    (0..N).fold(true, |all, index| all & window[index].is(literal[index]))
}

impl<'a, E: Encoding, R: Recorder> Parser<'a, E, R> {
    #[inline(always)]
    pub(super) fn run<T>(
        cx: &Cx,
        units: &[E::Unit],
        recorder: R,
        body: impl FnOnce(&mut Parser<'_, E, R>) -> T,
    ) -> T {
        cx.heap.collect_if_deferred();
        let _defer = cx.heap.defer_collection();
        let mut tail = [MaybeUninit::uninit(); 2 * WINDOW];
        body(&mut Parser {
            cx,
            rest: units,
            origin: units.len(),
            length: units.len(),
            tail: Some(&mut tail),
            stack: Vec::new(),
            keys: Vec::new(),
            has_repaired_text: false,
            recorder,
        })
    }

    pub(super) fn position(&self) -> usize {
        self.origin - self.rest.len()
    }

    /// Returns the position.
    pub(super) fn skip_white_space(&mut self) -> usize {
        self.rest = &self.rest[scan::json_white_space_len(self.rest)..];
        self.position()
    }

    /// Whether U+FFFD has taken the place of bytes that are not UTF-8.
    pub(super) fn has_repaired_text(&self) -> bool {
        self.has_repaired_text
    }

    /// A value that is all there is.
    #[inline]
    pub(super) fn text(&mut self) -> Result<Value, Failure> {
        let value = self.value()?;
        if self.skip_white_space() != self.length {
            return Err(self.unexpected(self.rest, Expected::EndOfText));
        }
        Ok(value)
    }

    #[cold]
    fn unexpected(&self, rest: &[E::Unit], expected: Expected) -> Failure {
        let at = self.origin - rest.len();
        Failure::Syntax(SyntaxError { at, expected })
    }

    #[cold]
    #[inline(never)]
    fn pad(&mut self, rest: &[E::Unit]) -> &'a [E::Unit] {
        let tail = self.tail.take().expect("no zero is consumed");
        self.origin += WINDOW;
        Cursor::fill_uninit(tail, |cursor| {
            cursor.push_slice(rest);
            cursor.push_array([E::Unit::default(); WINDOW]);
        })
    }

    #[inline(always)]
    fn window(&mut self, rest: &mut &'a [E::Unit]) -> &'a [E::Unit; WINDOW] {
        loop {
            match rest.first_chunk() {
                Some(window) => return window,
                None => *rest = self.pad(rest),
            }
        }
    }

    #[inline(always)]
    fn push(&mut self, value: Value) -> JsResult<()> {
        if self.stack.len() == self.stack.capacity() {
            self.grow_stack()?;
        }
        self.stack.push(value);
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn grow_stack(&mut self) -> JsResult<()> {
        if self.stack.len() >= u32::MAX as usize / 2 || self.stack.try_reserve(32).is_err() {
            return Err(self.cx.throw_out_of_memory());
        }
        Ok(())
    }

    #[inline(always)]
    fn enter(&mut self, scope: &mut Scope, kind: Kind) -> JsResult<()> {
        self.push(scope.to_value())?;
        let base = self.stack.len() as u32;
        *scope = Scope { base, kind };
        Ok(())
    }

    /// The next value. The position is right after it.
    pub(super) fn value(&mut self) -> Result<Value, Failure> {
        let cx = self.cx;
        let mut rest = self.rest;
        let mut expected = Expected::Value;
        let mut scope = Scope {
            base: 0,
            kind: Kind::Outside,
        };
        'value: loop {
            let window = self.window(&mut rest);
            let (mut value, length) = match byte(window[0]) {
                b'{' => {
                    self.recorder.open(cx)?;
                    rest = skip_white_space(&rest[1..]);
                    if !self.window(&mut rest)[0].is(b'}') {
                        self.enter(&mut scope, Kind::Object)?;
                        rest = self.key(rest, Expected::ClosingBrace)?;
                        expected = Expected::Value;
                        continue;
                    }
                    let object = cx.new_object().into();
                    self.recorder.close(cx, object, 0)?;
                    (object, 1)
                }
                b'[' => {
                    self.recorder.open(cx)?;
                    rest = skip_white_space(&rest[1..]);
                    if !self.window(&mut rest)[0].is(b']') {
                        self.enter(&mut scope, Kind::Array)?;
                        expected = Expected::Value;
                        continue;
                    }
                    let array = cx.new_array().into();
                    self.recorder.close(cx, array, 0)?;
                    (array, 1)
                }
                b' ' | b'\n' | b'\r' | b'\t' => {
                    rest = skip_white_space(rest);
                    continue;
                }
                first => {
                    let (value, length) = match first {
                        b'"' => self.string(rest)?,
                        b'-' | b'0'..=b'9' => match parse_json_number(rest) {
                            Ok((JsonNumber::Int32(number), length)) => (number.into(), length),
                            Ok((JsonNumber::Double(number), length)) => (number.into(), length),
                            Err(_) => return Err(self.unexpected(rest, expected)),
                        },
                        b't' if starts_with(window, b"true") => (Value::TRUE, 4),
                        b'f' if starts_with(window, b"false") => (Value::FALSE, 5),
                        b'n' if starts_with(window, b"null") => (Value::NULL, 4),
                        _ => return Err(self.unexpected(rest, expected)),
                    };
                    let start = self.origin - rest.len();
                    (self.recorder).primitive(cx, value, || start..start + length)?;
                    (value, length)
                }
            };
            rest = &rest[length..];
            while scope.kind != Kind::Outside {
                self.push(value)?;
                rest = skip_white_space(rest);
                let base = scope.base as usize;
                value = match (byte(self.window(&mut rest)[0]), scope.kind) {
                    (b',', Kind::Object) => {
                        rest = self.key(skip_white_space(&rest[1..]), Expected::Key)?;
                        expected = Expected::Value;
                        continue 'value;
                    }
                    (b',', _) => {
                        rest = &rest[1..];
                        expected = Expected::Element;
                        continue 'value;
                    }
                    (b'}', Kind::Object) => self.finish_object(base)?,
                    (b']', Kind::Array) => cx.new_array_from(&self.stack[base..])?.into(),
                    (_, Kind::Object) => return Err(self.unexpected(rest, Expected::ClosingBrace)),
                    _ => return Err(self.unexpected(rest, Expected::ClosingBracket)),
                };
                self.recorder.close(cx, value, self.stack.len() - base)?;
                rest = &rest[1..];
                scope = Scope::from_value(self.stack[base - 1]);
                self.stack.truncate(base - 1);
            }
            self.rest = rest;
            return Ok(value);
        }
    }

    /// Returns what follows the colon.
    fn key(&mut self, rest: &'a [E::Unit], expected: Expected) -> Result<&'a [E::Unit], Failure> {
        if !rest.first().is_some_and(|unit| unit.is(b'"')) {
            return Err(self.unexpected(rest, expected));
        }
        let (key, length) = self.with_characters(rest, |text| match parse_array_index(text) {
            Some(index) => Key::Index(index),
            None => Key::Name(Identifier::new(text)),
        })?;
        self.recorder.key(|| match &key {
            Key::Name(name) => name.clone(),
            Key::Index(index) => Identifier::from_index(*index),
        });
        (self.keys.try_reserve(1)).map_err(|_| self.cx.throw_out_of_memory())?;
        self.keys.push(key);
        match skip_white_space(&rest[length..]) {
            [colon, rest @ ..] if colon.is(b':') => Ok(rest),
            rest => Err(self.unexpected(rest, Expected::Colon)),
        }
    }

    fn finish_object(&mut self, base: usize) -> JsResult<Value> {
        let (cx, values) = (self.cx, &self.stack[base..]);
        let object = cx.new_object();
        let keys = self.keys.drain(self.keys.len() - values.len()..);
        for (key, &value) in keys.zip(values) {
            match key {
                Key::Name(name) => object.put_direct(cx, name.name(), value, Attributes::NONE),
                Key::Index(index) => cx.or_out_of_memory(object.store_element(cx, index, value))?,
            }
        }
        Ok(object.into())
    }

    /// The value and the length of the literal that `rest` starts with.
    fn string(&mut self, rest: &[E::Unit]) -> Result<(Value, usize), Failure> {
        let cx = self.cx;
        let (string, length) = self.with_characters(rest, StringRef::try_narrowed)?;
        Ok((
            JsString::new(cx, cx.or_out_of_memory(string)?).into(),
            length,
        ))
    }

    /// Lends the characters of the literal that `rest` starts with. Returns its length too.
    fn with_characters<T>(
        &mut self,
        rest: &[E::Unit],
        read: impl FnOnce(Text<'_>) -> T,
    ) -> Result<(T, usize), Failure> {
        const BYTES_ON_STACK: usize = 512;
        let mut end = 1 + scan::json_string_run_len(&rest[1..]);
        let run = E::Unit::text(&rest[1..end]);
        if rest.get(end).is_some_and(|unit| unit.is(b'"')) && (!E::IS_UTF8 || run.is_ascii()) {
            return Ok((read(run), end + 1));
        }
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
                self.cx.or_out_of_memory(Err(AllocError))?;
            }
            Ok((read(characters.text()), end + 1))
        })
    }

    fn decode(&mut self, run: &[E::Unit], characters: &mut StringBuilder) {
        match E::Unit::text(run) {
            Text::Latin1(bytes) if E::IS_UTF8 => {
                self.has_repaired_text |= !bun_simdutf_sys::simdutf::validate::utf8(bytes);
                characters.push_utf8_lossy(bytes);
            }
            text => characters.push_text(text),
        }
    }
}

#[inline(always)]
fn skip_white_space<T: CodeUnit>(units: &[T]) -> &[T] {
    &units[scan::json_white_space_len(units)..]
}
