//! The optimizing back end: SSA IR, instruction selection, register allocation, emission.

#![forbid(unsafe_code)]

pub mod analysis;
mod bitset;
mod client;
mod hash;
pub mod ir;

pub use bitset::BitSet;
pub use client::{CallSite, Client, ExitSite, Frame, PatchpointSite};
