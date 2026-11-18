//! Regular expressions: parser, pattern compiler and backtracking interpreter.

#![forbid(unsafe_code)]

mod error;
mod flags;
pub mod set;
pub mod tree;

pub use error::{Error, ErrorKind};
pub use flags::{Flags, FlagsText};
