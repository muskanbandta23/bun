//! Compiles WebAssembly functions to machine code.

#![forbid(unsafe_code)]

pub mod abi;
pub mod baseline;
pub mod optimize;
mod output;

pub use output::{CallSite, CompiledFunction, Counters, DirectCall, Environment, InlineFrame};
