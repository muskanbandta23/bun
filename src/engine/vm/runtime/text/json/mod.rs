//! JSON text (ECMA-404) to values and back.

mod lines;
mod parser;
mod raw;
mod records;
mod reviver;
mod stringify;
mod syntax_error;
mod token;

use bun_engine_text::Text;

use crate::heap::Survivors;
use crate::services::Service;
use crate::{Cx, JsResult, Thrown, Value, Visitor, Vm};
use parser::Parser;
use syntax_error::SyntaxError;

pub use lines::{LinesEnd, ParsedLines, parse_lines};
pub use raw::RawJson;
pub use reviver::parse_with_reviver;
pub use stringify::stringify;

#[derive(Default)]
pub(super) struct State {
    parser: parser::State,
}

impl Service for State {
    fn visit(&self, visitor: &mut Visitor) {
        self.parser.visit(visitor);
    }

    fn reconcile_weak_references(&self, vm: &Vm, survivors: &Survivors) {
        self.parser.reconcile_weak_references(vm, survivors);
    }
}

#[derive(Clone, Copy)]
pub enum Source<'a> {
    Text(Text<'a>),
    /// Stands for the text that the decoder of the Encoding Standard makes of the bytes.
    Utf8(&'a [u8]),
}

impl<'a> From<Text<'a>> for Source<'a> {
    #[inline(always)]
    fn from(text: Text<'a>) -> Source<'a> {
        Source::Text(text)
    }
}

impl Source<'_> {
    #[inline(always)]
    fn len(self) -> usize {
        with_units!(self, |units, _| units.len())
    }
}

/// Runs an expression over the units of a `Source` as a slice, and whether they are UTF-8, once for each encoding.
macro_rules! with_units {
    ($source:expr, |$units:ident, $is_utf8:pat_param| $body:expr) => {
        match $source {
            Source::Text(Text::Latin1($units)) => {
                let $is_utf8 = false;
                $body
            }
            Source::Text(Text::Utf16($units)) => {
                let $is_utf8 = false;
                $body
            }
            Source::Utf8($units) => {
                let $is_utf8 = true;
                $body
            }
        }
    };
}

use with_units;

enum Failure {
    Syntax(SyntaxError),
    Thrown(Thrown),
}

impl From<Thrown> for Failure {
    fn from(thrown: Thrown) -> Failure {
        Failure::Thrown(thrown)
    }
}

impl Failure {
    #[cold]
    fn throw(self, cx: &Cx, source: Source<'_>) -> Thrown {
        match self {
            Failure::Syntax(error) => error.throw(cx, source),
            Failure::Thrown(thrown) => thrown,
        }
    }
}

fn parse_text(cx: &Cx, source: Source<'_>) -> Result<Value, Failure> {
    Parser::run(cx, source, (), |parser, tables, cursor| {
        parser.text(tables, cursor)
    })
}

/// `JSON.parse` without a reviver.
pub fn parse(cx: &Cx, source: Source<'_>) -> JsResult<Value> {
    parse_text(cx, source).map_err(|failure| failure.throw(cx, source))
}

/// `None` if it is not JSON text.
pub fn try_parse(cx: &Cx, source: Source<'_>) -> JsResult<Option<Value>> {
    match parse_text(cx, source) {
        Ok(value) => Ok(Some(value)),
        Err(Failure::Syntax(_)) => Ok(None),
        Err(Failure::Thrown(thrown)) => Err(thrown),
    }
}
