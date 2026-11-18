//! Compiles WebAssembly functions to machine code.

#![forbid(unsafe_code)]

pub mod abi;
pub mod baseline;
pub mod cast;
pub mod optimize;
mod output;
pub mod tail_call;

pub use output::{CallSite, CompiledFunction, Counters, DirectCall, Environment, InlineFrame};
