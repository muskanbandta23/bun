//! Lexer, parser, scope analysis and early errors.

#![forbid(unsafe_code)]

pub mod ast;
mod cache;
mod chars;
mod error;
mod lexer;
mod lines;
mod names;
mod options;
mod parser;
mod scope;
mod source;
mod token;

pub use cache::SourceCache;
pub use error::{Error, ErrorKind, Recovery};
pub use lines::LineTable;
pub use names::{Name, NameKind, Names};
pub use options::{
    EvalContext, FieldKind, FieldSource, FunctionFlags, FunctionKind, FunctionSource, Goal, Options,
};
pub use parser::{Parser, PauseKind, PausePosition};

/// Empties a buffer for the next parse, and gives its memory back if that is a lot.
#[inline]
fn clear_buffer<T>(buffer: &mut Vec<T>) {
    const KEPT_BYTES: usize = 256 << 10;
    if buffer.capacity() * size_of::<T>() > KEPT_BYTES {
        *buffer = Vec::new();
    } else {
        buffer.clear();
    }
}
