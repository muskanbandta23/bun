//! Regular expressions: parser, pattern compiler and backtracking interpreter.

#![forbid(unsafe_code)]

mod construct;
mod error;
mod flags;
mod parser;
mod pattern;
pub mod set;
pub mod tree;

pub use construct::GroupNames;
pub use error::{Error, ErrorKind};
pub use flags::{Flags, FlagsText};
pub use parser::{Summary, check_syntax, summarize};
pub use pattern::Pattern;
