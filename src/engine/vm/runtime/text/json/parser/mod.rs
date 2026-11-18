mod keys;
mod literals;
mod machine;
mod objects;
mod reader;
mod stack;
mod strings;

use core::cell::{OnceCell, RefCell};
use core::mem::MaybeUninit;

use bun_alloc::AllocError;
use bun_engine_text::scan;

use super::records::Recorder;
use super::syntax_error::{Expected, SyntaxError};
use super::{Failure, Source};
use crate::heap::{Survivors, barrier};
use crate::services::Service;
use crate::vm::Root;
use crate::{Cx, Gc, ImmutableArray, JsArray, JsResult, Value, Visitor, Vm};
use keys::{KeyTree, NodeId};
use objects::Key;
use reader::{Cursor, Reader, WINDOW};
use stack::StackMemory;
use strings::Strings;

/// What one text leaves for the next.
#[derive(Default)]
pub(super) struct Tables {
    tree: KeyTree,
    strings: Strings,
    keys: Vec<Key>,
    stack: StackMemory,
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
            tables.stack = StackMemory::default();
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

/// No collection runs while it is alive: all that it makes is part of the result.
#[repr(C)]
pub(super) struct Parser<'a, R> {
    /// At the address of the parser, which is at hand.
    reader: Reader<'a>,
    cx: &'a Cx,
    root: NodeId,
    has_non_ascii_bytes: bool,
    /// Why a method has returned `None`.
    failure: Option<Failure>,
    pub(super) recorder: R,
}

impl<'a, R: Recorder> Parser<'a, R> {
    #[inline(always)]
    pub(super) fn run<T>(
        cx: &Cx,
        source: Source<'_>,
        recorder: R,
        body: impl for<'b> FnOnce(&mut Parser<'b, R>, &mut Tables, Cursor<'b>) -> T,
    ) -> T {
        let _defer = cx.heap.defer_collection_for(source.len());
        let tables = &cx.services.text.json.parser.tables;
        let mut tables = tables.get_or_init(Box::default).borrow_mut();
        let mut buffer = [MaybeUninit::uninit(); _];
        let (reader, cursor) = Reader::new(source, &mut buffer);
        let mut parser = Parser {
            cx,
            reader,
            root: tables.tree.root(&cx.global()),
            has_non_ascii_bytes: false,
            failure: None,
            recorder,
        };
        let result = body(&mut parser, &mut tables, cursor);
        tables.stack.trim();
        barrier::publish_fence();
        result
    }

    pub(super) fn position(&self, cursor: &Cursor<'a>) -> usize {
        self.reader.position(cursor)
    }

    /// Returns the position.
    #[inline(always)]
    pub(super) fn skip_white_space(&mut self, cursor: &mut Cursor<'a>) -> usize {
        loop {
            let window = self.reader.window(cursor);
            let blank = scan::json_white_space_len(window);
            cursor.advance(blank as u32);
            if blank != WINDOW {
                return self.position(cursor);
            }
        }
    }

    /// Whether a string has had any, in UTF-8.
    pub(super) fn has_non_ascii_bytes(&self) -> bool {
        self.has_non_ascii_bytes
    }

    /// A value that is all there is.
    #[inline(always)]
    pub(super) fn text(
        &mut self,
        tables: &mut Tables,
        mut cursor: Cursor<'a>,
    ) -> Result<Value, Failure> {
        self.skip_white_space(&mut cursor);
        let (mut cursor, value) = self.value(tables, cursor)?;
        let at = self.skip_white_space(&mut cursor);
        if at != self.reader.source().len() {
            let expected = Expected::EndOfText;
            return Err(Failure::Syntax(SyntaxError { at, expected }));
        }
        Ok(value)
    }

    /// The value at the cursor, and the cursor after it.
    #[inline(always)]
    pub(super) fn value(
        &mut self,
        tables: &mut Tables,
        mut cursor: Cursor<'a>,
    ) -> Result<(Cursor<'a>, Value), Failure> {
        let first = self.reader.window(&mut cursor)[0];
        let result = if first == b'{' || first == b'[' {
            self.structure(tables, cursor, first == b'{')
        } else {
            self.primitive(tables, cursor)
        };
        result.ok_or_else(|| {
            tables.keys.clear();
            self.failure.take().expect("a reason")
        })
    }

    #[inline(always)]
    fn primitive(
        &mut self,
        tables: &mut Tables,
        cursor: Cursor<'a>,
    ) -> Option<(Cursor<'a>, Value)> {
        let start = self.position(&cursor);
        let (cursor, value) = self.any_primitive(tables, cursor);
        let range = start..self.position(&cursor);
        let recorded = self.recorder.primitive(self.cx, value?, range);
        self.or_thrown(recorded)?;
        Some((cursor, value?))
    }

    #[cold]
    fn unexpected<T>(&mut self, cursor: &Cursor<'a>, expected: Expected) -> Option<T> {
        let at = self.position(cursor);
        self.failure = Some(Failure::Syntax(SyntaxError { at, expected }));
        None
    }

    #[cold]
    fn out_of_memory<T>(&mut self) -> Option<T> {
        self.failure = Some(self.cx.throw_out_of_memory().into());
        None
    }

    #[inline(always)]
    fn or_out_of_memory<T>(&mut self, result: Result<T, AllocError>) -> Option<T> {
        result.ok().or_else(|| self.out_of_memory())
    }

    #[inline(always)]
    fn or_thrown<T>(&mut self, result: JsResult<T>) -> Option<T> {
        result.map_err(|it| self.failure = Some(it.into())).ok()
    }
}
