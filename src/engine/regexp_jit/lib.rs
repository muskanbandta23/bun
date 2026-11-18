//! Machine code for compiled regular expressions.
//!
//! The code is a C function (System V on x86-64) that starts at its first byte:
//!
//! ```text
//! fn(subject: *const unit, start: usize, length: usize, captures: *mut u32, context: &mut Context) -> (usize, usize)
//! ```
//!
//! The subject has the width of the program and `length` units, at most `i32::MAX`. `start` fits in 32 bits.
//! `captures` is as for `Program::exec`. The results are the start and the end of the match, unless the low half of
//! the first is negative: see [`outcome`].

#![forbid(unsafe_code)]

mod back_reference;
mod call;
mod captures;
mod class;
mod code_point;
mod frame;
mod generator;
mod lanes;
mod run;
mod scan;
mod stack;
mod switch;
mod tables;
mod vector;

use bun_engine_asm::{Code, CodeTooLarge, Features};
use bun_engine_regexp::Outcome;
use bun_engine_regexp::program::Verified;

pub use tables::Tables;

pub const NO_MATCH: u32 = u32::MAX;
pub const INTERRUPTED: u32 = u32::MAX - 1;
pub const OUT_OF_MEMORY: u32 = u32::MAX - 2;

/// What the results of the code mean.
#[inline]
pub fn outcome(start: usize, end: usize) -> Outcome {
    match start as u32 {
        NO_MATCH => Outcome::NoMatch,
        INTERRUPTED => Outcome::Interrupted,
        OUT_OF_MEMORY => Outcome::OutOfMemory,
        start => Outcome::Match {
            start,
            end: end as u32,
        },
    }
}

#[derive(Clone, Copy)]
pub struct Options {
    pub features: Features,
    /// Decides how constants that the pattern has chosen are disguised.
    pub seed: u64,
    pub max_code_size: usize,
}

pub struct Compiled {
    pub code: Code,
    /// Has to live for as long as the code can run.
    pub tables: Tables,
}

pub fn compile(program: Verified<'_>, options: &Options) -> Result<Compiled, CodeTooLarge> {
    generator::generate(program, options)
}
