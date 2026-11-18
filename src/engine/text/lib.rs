//! Strings, atoms and symbols shared by the engine and its embedders.

mod code_unit;
mod hash;
mod view;

pub use code_unit::CodeUnit;
pub use hash::{hash_latin1, hash_utf16};
pub use view::{CodePoints, CodeUnits, Text};

/// The most code units a string has.
pub const MAX_LENGTH: usize = i32::MAX as usize;
