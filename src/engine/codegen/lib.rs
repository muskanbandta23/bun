//! The optimizing back end: SSA IR, instruction selection, register allocation, emission.

#![forbid(unsafe_code)]

pub mod analysis;
mod bitset;
mod client;
mod compiler;
mod hash;
pub mod ir;
mod machine;
pub mod passes;
mod target;
#[path = "x86_64/mod.rs"]
mod x64;

pub use bitset::BitSet;
pub use client::{CallSite, Client, ExitSite, Frame, PatchpointSite};
#[cfg(target_arch = "x86_64")]
pub use compiler::X64Compiler as Compiler;
pub use compiler::{Compiled, Options, TooComplex, X64Compiler};
pub use machine::Emitted;
pub use target::Target;
