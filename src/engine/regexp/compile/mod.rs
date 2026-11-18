//! From the tree to a program.

pub(crate) mod character;
mod emit;
pub(crate) mod graph;
mod groups;
mod lower;
mod lower_repeat;
pub(crate) mod memory;

use bun_engine_text::Text;

use crate::Error;
use crate::analysis::decide_backtracking;
use crate::program::{Program, Width};
use crate::tree::Tree;
use memory::StackLimit;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Finds where the match starts and ends.
    MatchOnly,
    Captures,
}

pub struct Options<'a> {
    pub width: Width,
    pub mode: Mode,
    /// A subject like those to come, or nothing.
    pub sample: Text<'a>,
    /// The lowest address that the native stack may reach.
    pub stack_limit: usize,
}

pub fn compile(tree: &Tree, options: &Options<'_>) -> Result<Program, Error> {
    let mut lowered = lower::lower(tree, options)?;
    let stack = StackLimit(options.stack_limit);
    decide_backtracking(&mut lowered.graph, lowered.root, stack)?;
    let program = emit::emit(tree, lowered, options)?;
    debug_assert!(program.verify().is_some());
    Ok(program)
}
