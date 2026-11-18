//! JSON text (ECMA-404) to values and back.

mod lines;
mod parser;
mod raw;
mod records;
mod reviver;
mod stringify;
mod syntax_error;
mod token;

use bun_engine_text::{CodeUnit, Text};

use crate::{Cx, JsResult, Thrown, Value};
use parser::Parser;
use syntax_error::SyntaxError;

pub use lines::{LinesEnd, ParsedLines, parse_lines};
pub use raw::RawJson;
pub use reviver::parse_with_reviver;
pub use stringify::stringify;

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

trait Encoding {
    type Unit: CodeUnit;
    const IS_UTF8: bool = false;
}

struct Latin1;
struct Utf16;
struct Utf8;

impl Encoding for Latin1 {
    type Unit = u8;
}

impl Encoding for Utf16 {
    type Unit = u16;
}

impl Encoding for Utf8 {
    type Unit = u8;
    const IS_UTF8: bool = true;
}

/// Calls a function that is generic over the `Encoding` with the units of a `Source` as its second argument.
macro_rules! with_source {
    ($source:expr, $function:ident($cx:expr, _ $(, $argument:expr)*)) => {
        match $source {
            Source::Text(Text::Latin1(units)) => $function::<Latin1>($cx, units $(, $argument)*),
            Source::Text(Text::Utf16(units)) => $function::<Utf16>($cx, units $(, $argument)*),
            Source::Utf8(units) => $function::<Utf8>($cx, units $(, $argument)*),
        }
    };
}

use with_source;

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

fn parse_units<E: Encoding>(cx: &Cx, units: &[E::Unit]) -> Result<Value, Failure> {
    Parser::<E, ()>::run(cx, units, (), |parser| parser.text())
}

/// `JSON.parse` without a reviver.
pub fn parse(cx: &Cx, source: Source<'_>) -> JsResult<Value> {
    with_source!(source, parse_units(cx, _)).map_err(|failure| failure.throw(cx, source))
}

/// `None` if it is not JSON text.
pub fn try_parse(cx: &Cx, source: Source<'_>) -> JsResult<Option<Value>> {
    match with_source!(source, parse_units(cx, _)) {
        Ok(value) => Ok(Some(value)),
        Err(Failure::Syntax(_)) => Ok(None),
        Err(Failure::Thrown(thrown)) => Err(thrown),
    }
}
