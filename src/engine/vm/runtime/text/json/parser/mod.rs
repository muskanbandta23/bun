mod keys;
mod literals;
mod objects;
mod stack;
mod strings;

use core::cell::{OnceCell, RefCell};
use core::mem::MaybeUninit;

use bun_engine_number::{JsonNumber, parse_json_number};
use bun_engine_text::{CodeUnit, Cursor, scan};

use super::records::Recorder;
use super::syntax_error::{Expected, SyntaxError};
use super::{Encoding, Failure};
use crate::heap::Survivors;
use crate::services::Service;
use crate::vm::Root;
use crate::{Cx, Gc, ImmutableArray, JsArray, JsFinalObject, JsResult, Value, Visitor, Vm};
use keys::{KeyTree, NodeId};
use objects::Key;
use stack::{Kind, Scope, Stack};
use strings::{SHORT, Strings};

/// What one text leaves for the next.
#[derive(Default)]
struct Tables {
    tree: KeyTree,
    strings: Strings,
    keys: Vec<Key>,
    stack: Stack,
}

pub(super) struct State {
    tables: OnceCell<Box<RefCell<Tables>>>,
    /// What empty arrays share until they are written to.
    no_elements: Root<ImmutableArray>,
}

impl Default for State {
    fn default() -> State {
        State {
            tables: OnceCell::new(),
            no_elements: Root::new(),
        }
    }
}

impl Service for State {
    fn visit(&self, visitor: &mut Visitor) {
        self.no_elements.visit(visitor);
    }

    fn reconcile_weak_references(&self, _: &Vm, survivors: &Survivors) {
        if let Some(tables) = self.tables.get() {
            let mut tables = tables.borrow_mut();
            tables.tree.reconcile(survivors);
            tables.strings.clear();
            tables.stack = Stack::default();
            tables.keys = Vec::new();
        }
    }
}

impl State {
    #[inline(always)]
    fn empty_array(&self, cx: &Cx) -> JsResult<Gc<JsArray>> {
        let elements = match self.no_elements.get() {
            Some(elements) => elements,
            None => {
                let elements = cx.or_out_of_memory(ImmutableArray::try_from_values(cx, &[]))?;
                self.no_elements.set(Some(elements));
                elements
            }
        };
        JsArray::from_immutable(cx, elements)
    }
}

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
    tables: &'a mut Tables,
    root: NodeId,
    has_non_ascii_bytes: bool,
    pub(super) recorder: R,
}

#[inline(always)]
fn byte<T: CodeUnit>(unit: T) -> u8 {
    unit.to_u16().min(0xff) as u8
}

#[inline(always)]
fn chunk<T, const N: usize>(window: &[T; WINDOW], at: usize) -> &[T; N] {
    window[at..].first_chunk().expect("inside the window")
}

#[inline(always)]
fn starts_with<E: Encoding>(window: &[E::Unit; WINDOW], at: usize, literal: &[u8; 4]) -> bool {
    E::bytes(chunk(window, at)) == *literal
}

#[inline(always)]
fn skip_white_space<T: CodeUnit>(units: &[T]) -> &[T] {
    &units[scan::json_white_space_len(units)..]
}

impl<'a, E: Encoding, R: Recorder> Parser<'a, E, R> {
    #[inline(always)]
    pub(super) fn run<T>(
        cx: &Cx,
        units: &[E::Unit],
        recorder: R,
        body: impl FnOnce(&mut Parser<'_, E, R>) -> T,
    ) -> T {
        let _defer = cx.heap.defer_collection_for(units.len());
        let tables = &cx.services.text.json.parser.tables;
        let mut tables = tables.get_or_init(Box::default).borrow_mut();
        let mut tail = [MaybeUninit::uninit(); 2 * WINDOW];
        body(&mut Parser {
            cx,
            rest: units,
            origin: units.len(),
            length: units.len(),
            tail: Some(&mut tail),
            root: tables.tree.root(&cx.global()),
            tables: &mut tables,
            has_non_ascii_bytes: false,
            recorder,
        })
    }

    pub(super) fn position(&self) -> usize {
        self.origin - self.rest.len()
    }

    /// Returns the position.
    #[inline(always)]
    pub(super) fn skip_white_space(&mut self) -> usize {
        self.rest = skip_white_space(self.rest);
        self.position()
    }

    /// Whether a string has had any, in UTF-8.
    pub(super) fn has_non_ascii_bytes(&self) -> bool {
        self.has_non_ascii_bytes
    }

    /// A value that is all there is.
    #[inline(always)]
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

    /// The window at the next token.
    #[inline(always)]
    fn token(&mut self, rest: &mut &'a [E::Unit]) -> &'a [E::Unit; WINDOW] {
        let window = self.window(rest);
        if window[0].to_u16() > b' '.into() {
            return window;
        }
        *rest = skip_white_space(rest);
        self.window(rest)
    }

    /// Reads a key and the colon if they are those of `foreseen`.
    #[inline(always)]
    fn foreseen_key(
        &self,
        window: &[E::Unit; WINDOW],
        rest: &mut &'a [E::Unit],
        foreseen: NodeId,
    ) -> bool {
        let bytes = [E::bytes(chunk(window, 0)), E::bytes(chunk(window, 16))];
        let found = self.tables.tree[foreseen].find_in(&bytes);
        found.map(|length| *rest = &rest[length..]).is_some()
    }

    #[inline(always)]
    fn string(
        &mut self,
        window: &[E::Unit; WINDOW],
        rest: &[E::Unit],
    ) -> Result<(Value, usize), Failure> {
        let bytes = E::bytes(chunk(window, 1));
        let length = (scan::mask(&bytes, E::ends_plain_run) | 1 << SHORT).trailing_zeros() as usize;
        if !window[1 + length].is(b'"') {
            return self.any_string(rest);
        }
        let string = self.tables.strings.short(self.cx, bytes, length);
        Ok((self.cx.or_out_of_memory(string)?.into(), length + 2))
    }

    /// The next value. The position is right after it.
    #[inline(always)]
    pub(super) fn value(&mut self) -> Result<Value, Failure> {
        let mut stack = core::mem::take(&mut self.tables.stack);
        let result = self.value_on(&mut stack);
        stack.clear();
        self.tables.stack = stack;
        if result.is_err() {
            self.tables.keys.clear();
        }
        result
    }

    #[inline(always)]
    fn value_on(&mut self, stack: &mut Stack) -> Result<Value, Failure> {
        let cx = self.cx;
        let mut rest = self.rest;
        let mut scope = Scope::outside(self.root);
        'value: loop {
            let window = self.window(&mut rest);
            let (mut value, length) = match byte(window[0]) {
                b'{' => {
                    self.recorder.open(cx)?;
                    rest = &rest[1..];
                    let window = self.token(&mut rest);
                    if !window[0].is(b'}') {
                        let context = scope.node();
                        let mut first = self.tables.tree[context].child;
                        if !self.foreseen_key(window, &mut rest, first) {
                            let expected = Expected::ClosingBrace;
                            let length;
                            (length, first) = self.unforeseen_key(rest, self.root, expected)?;
                            rest = &rest[length..];
                            if first != NodeId::OUTSIDE {
                                self.tables.tree[context].child = first;
                            }
                        }
                        self.recorder.key(|| self.tables.identifier(first));
                        scope = stack.enter(cx, scope, Kind::Object, first)?;
                        continue;
                    }
                    let object = cx.new_object().into();
                    self.recorder.close(cx, object, 0)?;
                    (object, 1)
                }
                b'[' => {
                    self.recorder.open(cx)?;
                    rest = &rest[1..];
                    if !self.token(&mut rest)[0].is(b']') {
                        scope = stack.enter(cx, scope, Kind::Array, scope.node())?;
                        continue;
                    }
                    let array = cx.services.text.json.parser.empty_array(cx)?.into();
                    self.recorder.close(cx, array, 0)?;
                    (array, 1)
                }
                b' ' | b'\n' | b'\r' | b'\t' => {
                    rest = skip_white_space(&rest[1..]);
                    continue;
                }
                first => {
                    let (value, length) = match first {
                        b'"' => self.string(window, rest)?,
                        b'-' | b'0'..=b'9' => match parse_json_number(rest) {
                            Ok((JsonNumber::Int32(number), length)) => (number.into(), length),
                            Ok((JsonNumber::Double(number), length)) => (number.into(), length),
                            Err(_) => return Err(self.unexpected(rest, Expected::Value)),
                        },
                        b't' if starts_with::<E>(window, 0, b"true") => (Value::TRUE, 4),
                        b'f' if starts_with::<E>(window, 1, b"alse") => (Value::FALSE, 5),
                        b'n' if starts_with::<E>(window, 0, b"null") => (Value::NULL, 4),
                        _ if scope.kind() == Kind::Array => {
                            return Err(self.unexpected(rest, Expected::Element));
                        }
                        _ => return Err(self.unexpected(rest, Expected::Value)),
                    };
                    let start = self.origin - rest.len();
                    (self.recorder).primitive(cx, value, || start..start + length)?;
                    (value, length)
                }
            };
            rest = &rest[length..];
            while scope.kind() != Kind::Outside {
                stack.push(cx, value)?;
                let (kind, node) = (scope.kind(), scope.node());
                value = match byte(self.token(&mut rest)[0]) {
                    b',' if kind == Kind::Array => {
                        rest = &rest[1..];
                        continue 'value;
                    }
                    b',' => {
                        rest = &rest[1..];
                        let window = self.token(&mut rest);
                        let mut next = self.tables.tree[node].next;
                        if !self.foreseen_key(window, &mut rest, next) {
                            let length;
                            (length, next) = self.unforeseen_key(rest, node, Expected::Key)?;
                            rest = &rest[length..];
                            if next != NodeId::OUTSIDE {
                                self.tables.tree[node].next = next;
                            }
                        }
                        self.recorder.key(|| self.tables.identifier(next));
                        scope.set_node(next);
                        continue 'value;
                    }
                    b'}' if kind == Kind::Object => {
                        let members = stack.members(scope);
                        let object = match self.tables.tree[node].shape() {
                            Some(shape) => JsFinalObject::try_create_with_values(cx, shape, members)
                                .map(Gc::upcast),
                            None => self.tables.create_object(cx, node, members),
                        };
                        cx.or_out_of_memory(object)?.into()
                    }
                    b']' if kind == Kind::Array => cx.new_array_from(stack.members(scope))?.into(),
                    _ if kind == Kind::Object => {
                        return Err(self.unexpected(rest, Expected::ClosingBrace));
                    }
                    _ => return Err(self.unexpected(rest, Expected::ClosingBracket)),
                };
                (self.recorder).close(cx, value, stack.members(scope).len())?;
                rest = &rest[1..];
                scope = stack.leave(scope);
            }
            self.rest = rest;
            return Ok(value);
        }
    }
}
