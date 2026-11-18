//! Letting the interpreter run one instruction on a frame that compiled code holds.

use bun_engine_codegen::ir::{Type, Value as Ssa};
use bun_engine_interpreter::OPERATIONS;

use super::Lowering;
use super::runtime_calls::Call;
use crate::builder::{Built, Refusal};

impl Lowering<'_> {
    /// The area that stands in for the frame of an inlined function. One serves the whole function, since no two
    /// interpreted instructions are at the same place.
    pub(super) fn shadow_frame(&mut self) -> Ssa {
        let area = self.graph.shadow_frame;
        let slot = *self.shadow.get_or_insert_with(|| {
            let words = (area.highest - area.lowest) as u32 + 1;
            self.graph.function.add_slot(words * 8, 8)
        });
        let base = self.graph.function.slot_address(slot);
        self.ir().add(base, i64::from(-area.lowest) * 8)
    }

    /// Calls the interpreter's whole-instruction path, which gives where control goes and a value for a return.
    pub(super) fn interpret(&mut self, opcode: u32) -> Built {
        let Some(operation) = OPERATIONS[opcode as usize] else {
            return Err(Refusal::Unsupported);
        };
        let (area, start) = (self.operand(0), self.operand(1));
        let vm = self.vm();
        let outcome = self.call_giving(
            operation as usize,
            &[vm, area, start],
            &[Type::I64, Type::I64],
            Call::Operation,
        );
        self.graph.function.replace(self.at, outcome);
        Ok(())
    }
}
