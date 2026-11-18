//! Compiles WebAssembly functions to machine code.

#![forbid(unsafe_code)]

pub mod abi;
mod output;

pub use output::{CallSite, CompiledFunction, DirectCall, Environment, InlineFrame, LoopEntry};
