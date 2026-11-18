//! Letting the interpreter run one instruction on a frame that compiled code holds.

use bun_engine_codegen::ir::Value as Ssa;

use super::Lowering;
use crate::builder::{Built, Refusal};

impl Lowering<'_> {
    pub(super) fn shadow_frame(&mut self) -> Ssa {
        Ssa::NONE
    }

    pub(super) fn interpret(&mut self, _opcode: u32) -> Built {
        Err(Refusal::Unsupported)
    }
}
