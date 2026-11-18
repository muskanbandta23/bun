//! The code that walks the lists of cases.
//!
//! An entry is called with the call site stored and the stack pointer aligned. It returns zero in `RESULT`, having
//! done nothing and kept the other registers of [`abi`], if no case applies. It overwrites all registers that are
//! not pinned. An accessor returns straight to the caller, which restores the stack pointer.

#![forbid(unsafe_code)]

mod elements;
mod entries;
mod handlers;
mod megamorphic;

use bun_engine_vm::Vm;
use bun_engine_vm::ic::{AccessCase, Key};

pub(crate) use elements::{Elements, load_element, store_element};
pub use entries::Entries;
pub(crate) use handlers::emit_handlers;

pub mod abi {
    use bun_engine_asm::Gpr;

    use crate::registers::*;

    pub const RESULT: Gpr = T0;
    pub const BASE: Gpr = T1;
    /// The address of a name that is part of the instruction, or else the value of the key.
    pub const KEY: Gpr = T2;
    /// What to store, the receiver, or the prototype to look for.
    pub const VALUE: Gpr = T6;
    /// The address of what the entry starts from.
    pub const ENTRY: Gpr = T7;

    pub(super) const CASE: Gpr = T4;
    pub(super) const SHAPE: Gpr = T5;
    /// The address of the name.
    pub(super) const NAME: Gpr = T3;
}

pub fn access_handler(_: &Vm, case: &AccessCase, key: Key) -> usize {
    crate::thunks::get().map_or(0, |thunks| {
        thunks.handlers[handlers::index(case.kind(), key)]
    })
}

pub fn entries() -> Option<&'static Entries<usize>> {
    crate::thunks::get().map(|thunks| &thunks.entries)
}
