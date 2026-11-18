//! Compiles a function, and some that it calls, through the optimizing back end.

mod feedback;

pub use bun_engine_codegen::TooComplex;
pub use bun_engine_codegen::passes::Level;
use bun_engine_wasm_format::CodeIndex;

pub use self::feedback::{CallFeedback, Feedback, InliningBudget};
use crate::output::{CompiledFunction, Environment};

/// Memory that the compilation of one function leaves to the next.
#[derive(Default)]
pub struct Compiler {}

impl Compiler {
    /// With `loop_entry` the code is entered at the start of that loop and takes over a [`crate::baseline::Frame`].
    pub fn compile(
        &mut self,
        _env: &Environment<'_>,
        _code: CodeIndex,
        _loop_entry: Option<u32>,
        _level: Level,
        _feedback: &dyn Feedback,
    ) -> Result<CompiledFunction, TooComplex> {
        Err(TooComplex)
    }
}
