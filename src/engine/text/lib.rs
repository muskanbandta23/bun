//! Strings, atoms and symbols shared by the engine and its embedders.

#![feature(core_io, core_io_borrowed_buf, maybe_uninit_fill)]

mod ascii;
mod atom_table;
mod block;
mod builder;
mod code_unit;
mod compare;
mod copy;
mod cursor;
mod hash;
mod identifier;
mod json;
pub mod scan;
mod search;
mod search_matches;
mod search_two_way;
mod search_units;
mod static_names;
mod string_impl;
mod string_ref;
mod symbol;
mod trim;
mod utf16;
mod utf8;
mod utf8_encode;
mod view;

pub use builder::StringBuilder;
pub use bun_alloc::AllocError;
pub use code_unit::CodeUnit;
pub use compare::{common_prefix_len, equal_units};
pub use cursor::Cursor;
pub use hash::{hash_latin1, hash_utf16};
pub use identifier::{Atom, Identifier, MAX_ARRAY_INDEX, PropertyName, parse_array_index};
pub use search_matches::Matches;
pub use static_names::{
    STATIC_ATOM_COUNT, find_private_name, find_well_known_symbol, names, private_names, symbols,
};
pub use string_impl::{FreeExternalBuffer, StaticString, StringImpl};
pub use string_ref::{IsolatedString, ReverseStringWriter, SharedString, StringRef, StringWriter};
pub use symbol::{
    OFFSET_OF_SYMBOL_FLAGS, OFFSET_OF_SYMBOL_HASH, StaticSymbol, SymbolFlags, SymbolRef,
};
pub use utf8_encode::Progress;
pub use view::{CodePoints, CodeUnits, Text};

/// The most code units a string has.
pub const MAX_LENGTH: usize = i32::MAX as usize;
