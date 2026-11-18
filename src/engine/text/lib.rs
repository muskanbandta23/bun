//! Strings, atoms and symbols shared by the engine and its embedders.

#![feature(maybe_uninit_fill)]

mod atom_table;
mod builder;
mod code_unit;
mod hash;
mod identifier;
mod search;
mod static_names;
mod string_impl;
mod string_ref;
mod symbol;
mod utf8;
mod view;

pub use builder::StringBuilder;
pub use bun_alloc::AllocError;
pub use code_unit::CodeUnit;
pub use hash::{hash_latin1, hash_utf16};
pub use identifier::{Atom, Identifier, MAX_ARRAY_INDEX, PropertyName, parse_array_index};
pub use static_names::{
    STATIC_ATOM_COUNT, find_private_name, find_well_known_symbol, names, private_names, symbols,
};
pub use string_impl::{FreeExternalBuffer, StaticString, StringImpl};
pub use string_ref::{IsolatedString, ReverseStringWriter, SharedString, StringRef, StringWriter};
pub use symbol::{
    OFFSET_OF_SYMBOL_FLAGS, OFFSET_OF_SYMBOL_HASH, StaticSymbol, SymbolFlags, SymbolRef,
};
pub use view::{CodePoints, CodeUnits, Text};

/// The most code units a string has.
pub const MAX_LENGTH: usize = i32::MAX as usize;
