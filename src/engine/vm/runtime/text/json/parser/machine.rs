//! The loop that all text goes through. What is rare or long is left to functions that preserve its registers.

use bun_engine_number::parse_few_json_digits;
use bun_engine_text::scan;

use super::keys::NodeId;
use super::reader::{Cursor, WINDOW};
use super::stack::Stack;
use super::strings::{SHORT, ShortText};
use super::{Parser, Tables};
use crate::json::records::Recorder;
use crate::json::syntax_error::Expected;
use crate::{ElementKind, JsArray, JsResult, Value};

#[derive(Clone, Copy)]
enum State {
    OpenObject,
    OpenArray,
    Member,
    Element,
    AfterMember,
    AfterElement,
    NextMember,
    NextElement,
    CloseObject,
    CloseArray,
    /// An object or an array is complete.
    Resume,
}

/// In the spare bits of a node: what the value of that key, or a part of it, is in.
const IN_OBJECT: u32 = 0;
const IN_ARRAY: u32 = 1;
const IN_NOTHING: u32 = 2;

type Window = [u8; WINDOW];

/// The compiler does not know that they are where it has loaded from before. Else it would put a vector together from
/// what it has, or take one apart, piece by piece.
#[inline(always)]
fn afresh<const N: usize>(bytes: &[u8]) -> &[u8; N] {
    let mut start = bytes[..N].as_ptr();
    // SAFETY: no instruction, so it points where it did.
    unsafe {
        core::arch::asm!("/* {0} */", inout(reg) start, options(pure, nomem, nostack, preserves_flags));
        &*start.cast()
    }
}

#[inline(always)]
fn has(window: &Window, at: usize, word: &[u8; 4]) -> bool {
    afresh(&window[at..]) == word
}

#[inline(always)]
fn ends_ascii_run(byte: u8) -> bool {
    ((byte ^ 2) as i8 <= 0x20) | (byte == b'\\')
}

#[inline(always)]
fn is_white_space(byte: u8) -> bool {
    (byte == b' ') | (byte == b'\n') | (byte == b'\r') | (byte == b'\t')
}

/// `lowest` is the least of the bits of the elements, none of which is NaN.
#[inline(always)]
fn kind_for(lowest: u64) -> ElementKind {
    if lowest < Value::DOUBLE_OFFSET {
        ElementKind::Contiguous
    } else if lowest < Value::NUMBER_TAG {
        ElementKind::Double
    } else {
        ElementKind::Int32
    }
}

impl<'a, R: Recorder> Parser<'a, R> {
    #[inline(never)]
    extern "rust-cold" fn skip_much_white_space(&mut self, mut cursor: Cursor<'a>) -> Cursor<'a> {
        self.skip_white_space(&mut cursor);
        cursor
    }

    /// The window after any white space.
    #[inline(always)]
    fn token(&mut self, mut cursor: Cursor<'a>) -> (Cursor<'a>, &Window) {
        let window = self.reader.window(&mut cursor);
        let blank = scan::mask(afresh(window), is_white_space);
        cursor.advance(blank.trailing_ones());
        if blank == 0xffff {
            cursor = self.skip_much_white_space(cursor);
        }
        let window = self.reader.window(&mut cursor);
        (cursor, window)
    }

    /// The same, for a state that starts over, to which the constants would be moved.
    #[inline(never)]
    extern "rust-cold" fn skip_to_token(&mut self, cursor: Cursor<'a>) -> Cursor<'a> {
        self.token(cursor).0
    }

    /// The length of the key of `node` with all that goes with it, if the window starts with that. It starts with a quote.
    #[inline(always)]
    fn find_key(tables: &Tables, window: &Window, node: NodeId) -> Option<u32> {
        tables.tree[node].find_in({
            let [low, high] = afresh::<32>(window).as_chunks().0 else {
                unreachable!()
            };
            [low, high]
        })
    }

    #[inline(never)]
    extern "rust-cold" fn empty_object(&mut self) -> Value {
        self.cx.new_object().into()
    }

    #[inline(never)]
    extern "rust-cold" fn empty_array(&mut self) -> Option<Value> {
        let array = self.cx.services.text.json.parser.empty_array(self.cx);
        self.or_thrown(array).map(Into::into)
    }

    #[inline(never)]
    extern "rust-cold" fn any_array(&mut self, elements: &[Value]) -> Option<Value> {
        let array = self.cx.new_array_from(elements);
        self.or_thrown(array).map(Into::into)
    }

    #[cold]
    #[inline(never)]
    extern "rust-cold" fn fail_at(&mut self, cursor: Cursor<'a>, expected: Expected) {
        self.unexpected::<()>(&cursor, expected);
    }

    #[inline(always)]
    fn record(&mut self, action: impl FnOnce(&mut R, &crate::Cx) -> JsResult<()>) -> Option<()> {
        let result = action(&mut self.recorder, self.cx);
        self.or_thrown(result)
    }

    /// The object or the array at the cursor, and the cursor after it.
    #[inline(always)]
    pub(super) fn structure(
        &mut self,
        tables: &mut Tables,
        mut cursor: Cursor<'a>,
        is_object: bool,
    ) -> Option<(Cursor<'a>, Value)> {
        let (vm, global) = (self.cx.vm(), self.cx.global());
        // SAFETY: a parser has the tables to itself, and defers collection.
        let stack = unsafe { Stack::new(&mut tables.stack) };
        let mut stack = self.or_out_of_memory(stack)?;
        let mut node = self.root;
        let mut context = node.bits() | IN_NOTHING;
        let mut value = Value::NULL;
        let mut lowest = 0;
        let mut state = if is_object {
            State::OpenObject
        } else {
            State::OpenArray
        };

        macro_rules! go {
            ($label:lifetime, $state:ident) => {{
                #[const_continue]
                break $label State::$state;
            }};
        }

        macro_rules! grow {
            ($result:expr) => {
                if $result.is_err() {
                    return self.out_of_memory();
                }
            };
        }

        macro_rules! read_value {
            ($label:lifetime, $this:ident, $after:ident, $next:ident, $close:ident, $closer:literal, $holder:expr, $expected:expr) => {{
                let start = R::IS_ACTIVE.then(|| self.reader.position(&cursor));
                let window = self.reader.window(&mut cursor);
                let first = window[0];
                let slow;
                if first == b'"' {
                    let characters = afresh(&window[1..]);
                    let stops = scan::mask(characters, ends_ascii_run) | 1 << SHORT;
                    let length = stops.trailing_zeros();
                    if window[1 + length as usize] == b'"' {
                        let text = ShortText::new(afresh(characters), length);
                        cursor.advance(length + 2);
                        value = match tables.strings.find_short(text) {
                            Some(string) => string.into(),
                            None => self.new_short_string(tables, text)?,
                        };
                    } else {
                        (cursor, slow) = self.any_string(tables, cursor);
                        value = slow?;
                    }
                } else if first.is_ascii_digit() {
                    if let Some((number, length)) = parse_few_json_digits(*afresh(window))
                        && !R::IS_ACTIVE
                    {
                        let following = window[length as usize];
                        value = Value::int32(number);
                        if following == b',' {
                            cursor.advance(length);
                            go!($label, $next)
                        }
                        if following == $closer {
                            cursor.advance(length);
                            go!($label, $close)
                        }
                        if following <= b' ' {
                            cursor.advance(length);
                            go!($label, $after)
                        }
                    }
                    (cursor, slow) = self.any_number(cursor);
                    value = slow?;
                } else if first == b'{' {
                    context = node.bits() | $holder;
                    go!($label, OpenObject)
                } else if first == b'[' {
                    context = node.bits() | $holder;
                    go!($label, OpenArray)
                } else if first == b' ' {
                    cursor.advance(1);
                    go!($label, $this)
                } else if has(window, 0, b"null") {
                    cursor.advance(4);
                    value = Value::NULL;
                } else if has(window, 0, b"true") {
                    cursor.advance(4);
                    value = Value::TRUE;
                } else if first == b'f' && has(window, 1, b"alse") {
                    cursor.advance(5);
                    value = Value::FALSE;
                } else if first == b'-' {
                    (cursor, slow) = self.any_number(cursor);
                    value = slow?;
                } else if is_white_space(first) {
                    cursor = self.skip_to_token(cursor);
                    go!($label, $this)
                } else {
                    self.fail_at(cursor, $expected);
                    return None;
                }
                if let Some(start) = start {
                    let end = self.reader.position(&cursor);
                    self.record(|it, cx| it.primitive(cx, value, start..end))?;
                }
                go!($label, $after)
            }};
        }

        #[loop_match]
        loop {
            state = 'next: {
                match state {
                    State::OpenObject => {
                        self.record(R::open)?;
                        cursor.advance(1);
                        let mut window = self.reader.window(&mut cursor);
                        if window[0] != b'"' {
                            (cursor, window) = self.token(cursor);
                            if window[0] == b'}' {
                                cursor.advance(1);
                                value = self.empty_object();
                                self.record(|it, cx| it.close(cx, value, 0))?;
                                go!('next, Resume)
                            }
                            if window[0] != b'"' {
                                self.fail_at(cursor, Expected::ClosingBrace);
                                return None;
                            }
                        }
                        // SAFETY: from `bits`.
                        let outer = unsafe { NodeId::from_bits(context & !NodeId::SPARE_BITS) };
                        node = tables.tree[outer].child;
                        if let Some(length) = Self::find_key(tables, window, node) {
                            cursor.advance(length);
                        } else {
                            (cursor, node) = self.unforeseen_first_key(tables, cursor, outer);
                            if node == NodeId::NONE {
                                return None;
                            }
                        }
                        self.recorder.key(|| tables.identifier(node));
                        grow!(stack.enter(&mut tables.stack, context));
                        go!('next, Member)
                    }
                    State::OpenArray => {
                        self.record(R::open)?;
                        cursor.advance(1);
                        let mut window = self.reader.window(&mut cursor);
                        if window[0] <= b' ' {
                            (cursor, window) = self.token(cursor);
                        }
                        if window[0] == b']' {
                            cursor.advance(1);
                            value = self.empty_array()?;
                            self.record(|it, cx| it.close(cx, value, 0))?;
                            go!('next, Resume)
                        }
                        // SAFETY: as above.
                        node = unsafe { NodeId::from_bits(context & !NodeId::SPARE_BITS) };
                        lowest = u64::MAX;
                        grow!(stack.enter(&mut tables.stack, context));
                        go!('next, Element)
                    }
                    State::Member => read_value!(
                        'next, Member, AfterMember, NextMember, CloseObject, b'}', IN_OBJECT, Expected::Value
                    ),
                    State::Element => read_value!(
                        'next, Element, AfterElement, NextElement, CloseArray, b']', IN_ARRAY, Expected::Element
                    ),
                    State::AfterMember => {
                        let mut window = self.reader.window(&mut cursor);
                        if window[0] == b',' {
                            go!('next, NextMember)
                        }
                        if window[0] == b'}' {
                            go!('next, CloseObject)
                        }
                        (cursor, window) = self.token(cursor);
                        if window[0] == b'}' {
                            go!('next, CloseObject)
                        }
                        if window[0] == b',' {
                            go!('next, NextMember)
                        }
                        self.fail_at(cursor, Expected::ClosingBrace);
                        return None;
                    }
                    State::AfterElement => {
                        let mut window = self.reader.window(&mut cursor);
                        if window[0] == b',' {
                            go!('next, NextElement)
                        }
                        if window[0] == b']' {
                            go!('next, CloseArray)
                        }
                        (cursor, window) = self.token(cursor);
                        if window[0] == b']' {
                            go!('next, CloseArray)
                        }
                        if window[0] == b',' {
                            go!('next, NextElement)
                        }
                        self.fail_at(cursor, Expected::ClosingBracket);
                        return None;
                    }
                    State::NextMember => {
                        grow!(stack.push(&mut tables.stack, value));
                        let next = tables.tree[node].next;
                        cursor.advance(1);
                        let mut window = self.reader.window(&mut cursor);
                        if window[0] != b'"' {
                            (cursor, window) = self.token(cursor);
                            if window[0] != b'"' {
                                self.fail_at(cursor, Expected::Key);
                                return None;
                            }
                        }
                        if let Some(length) = Self::find_key(tables, window, next) {
                            cursor.advance(length);
                            node = next;
                        } else {
                            (cursor, node) = self.unforeseen_next_key(tables, cursor, node);
                            if node == NodeId::NONE {
                                return None;
                            }
                        }
                        self.recorder.key(|| tables.identifier(node));
                        go!('next, Member)
                    }
                    State::NextElement => {
                        lowest = lowest.min(value.to_bits());
                        grow!(stack.push(&mut tables.stack, value));
                        cursor.advance(1);
                        go!('next, Element)
                    }
                    State::CloseObject => {
                        cursor.advance(1);
                        let members = stack.members_and(value);
                        let snug = tables.tree[node].snug_shape();
                        // SAFETY: there is a value for each key.
                        value = match snug.and_then(|it| unsafe { it.try_create(vm, members) }) {
                            Some(object) => object.into(),
                            None => self.any_object(tables, node, members)?,
                        };
                        self.record(|it, cx| it.close(cx, value, members.len()))?;
                        // SAFETY: what has been entered on the way here.
                        context = unsafe { stack.leave() };
                        go!('next, Resume)
                    }
                    State::CloseArray => {
                        cursor.advance(1);
                        let kind = kind_for(lowest.min(value.to_bits()));
                        let elements = stack.members_and(value);
                        // SAFETY: the kind goes by all of them.
                        value =
                            match unsafe { JsArray::try_create_small(vm, &global, kind, elements) }
                            {
                                Some(array) => array.into(),
                                None => self.any_array(elements)?,
                            };
                        self.record(|it, cx| it.close(cx, value, elements.len()))?;
                        // SAFETY: as above.
                        context = unsafe { stack.leave() };
                        go!('next, Resume)
                    }
                    State::Resume => {
                        // SAFETY: from `bits`.
                        node = unsafe { NodeId::from_bits(context & !NodeId::SPARE_BITS) };
                        if context & NodeId::SPARE_BITS == IN_OBJECT {
                            go!('next, AfterMember)
                        }
                        if context & IN_ARRAY != 0 {
                            lowest = value.to_bits();
                            go!('next, AfterElement)
                        }
                        return Some((cursor, value));
                    }
                }
            }
        }
    }
}
