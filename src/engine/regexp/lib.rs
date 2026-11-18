//! Regular expressions: parser, pattern compiler and backtracking interpreter.

#![forbid(unsafe_code)]

mod analysis;
mod compile;
mod construct;
mod context;
mod error;
mod flags;
mod interpreter;
mod parser;
mod pattern;
pub mod program;
mod search;
pub mod set;
mod surrogate;
pub mod tree;

pub use compile::{Mode, Options, compile};
pub use construct::GroupNames;
pub use context::{Context, FUEL, Outcome, helper};
pub use error::{Error, ErrorKind};
pub use flags::{Flags, FlagsText};
pub use parser::{Summary, check_syntax, summarize};
pub use pattern::Pattern;
pub use program::{Program, Width};
