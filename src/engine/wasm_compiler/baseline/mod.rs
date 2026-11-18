//! Compiles a function in one pass over its instructions.

mod frame;

use bun_engine_asm::CodeTooLarge;
use bun_engine_wasm_format::CodeIndex;

pub use self::frame::Frame;
use crate::output::{CompiledFunction, Counters, Environment};

/// Memory that the compilation of one function leaves to the next.
#[derive(Default)]
pub struct Compiler {}

impl Compiler {
    /// Without `counters` the code never asks for better code. With `loop_entry` it is entered at the start of that
    /// loop and takes over a [`Frame`].
    pub fn compile(
        &mut self,
        _env: &Environment<'_>,
        _code: CodeIndex,
        _counters: Option<Counters>,
        _loop_entry: Option<u32>,
    ) -> Result<CompiledFunction, CodeTooLarge> {
        Err(CodeTooLarge)
    }
}
