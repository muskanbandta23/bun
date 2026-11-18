//! Lexer, parser, scope analysis and early errors.

#![forbid(unsafe_code)]
#![allow(dead_code)]

pub mod ast;
mod names;
mod options;
mod scope;

pub use names::{Name, NameKind, Names};
pub use options::{
    EvalContext, FieldKind, FieldSource, FunctionFlags, FunctionKind, FunctionSource, Goal, Options,
};
