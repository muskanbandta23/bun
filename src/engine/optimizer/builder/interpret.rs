//! Has the interpreter execute an instruction, on the registers that it names.
//!
//! The frame of the code keeps the slots of the bytecode frame of the function that it was made from, so an
//! instruction of that function runs on the frame itself. One of a function that was inlined runs on an area that
//! looks like a frame, whose header and arguments the code fills in.

use bun_engine_bytecode::{EdgeKind, Flow, RawInstruction, Register, Shape, Written};
use bun_engine_codegen::ir::{State, Type, Value as Ssa};
use bun_engine_interpreter::outcome;
use bun_engine_vm::{CallFrame, SpeculatedType as T};
use smallvec::SmallVec;

use super::Builder;
use super::frame::ArgumentCount;
use crate::graph::{ARGUMENT_COUNT, ExitKind, Kind, heaps};

const CODE_BLOCK: Register = Register::from_offset(CallFrame::OFFSET_OF_CODE_BLOCK as i32 / 8);

impl Builder<'_> {
    pub(super) fn interpret_or_exit(&mut self, metadata: u32) {
        if self.prediction_or_exit(metadata).is_some() {
            self.interpret();
        }
    }

    pub(super) fn frame_pointer(&mut self) -> Ssa {
        self.graph
            .function
            .pinned(bun_engine_asm::abi::FRAME_POINTER)
    }

    /// Where the interpreter finds the registers of the current function.
    fn registers(&mut self) -> Ssa {
        if self.frame().is_root() {
            return self.frame_pointer();
        }
        let area = self.insert_node(Kind::ShadowFrame, &[], 0, State::NONE);
        let code_block = self.word(self.frame().code_block.address());
        let header = [
            (CODE_BLOCK, code_block),
            (Register::CALLEE, self.frame().get(Register::CALLEE)),
        ];
        for (register, value) in header {
            self.put(area, register, value);
        }
        if let ArgumentCount::Known(count) = self.frame().argument_count {
            let count = self.word(count as usize);
            self.put(area, ARGUMENT_COUNT, count);
            for register in self.frame().argument_registers() {
                self.put(area, register, self.frame().original_argument(register));
            }
        }
        area
    }

    fn put(&mut self, area: Ssa, register: Register, value: Ssa) {
        self.graph.shadow_frame.include(register);
        let value = self.graph.root(value);
        self.ir()
            .store(value, area, register.offset() * 8, heaps::FRAME);
    }

    fn take(&mut self, area: Ssa, register: Register) -> Ssa {
        self.graph.shadow_frame.include(register);
        self.ir()
            .load(Type::I64, area, register.offset() * 8, heaps::FRAME)
    }

    pub(super) fn interpret(&mut self) {
        let frame = self.frame();
        let (code_block, offset) = (frame.code_block, frame.offset);
        let body = code_block.body();
        let instruction = RawInstruction::read(body.instructions(), offset).expect("verified");
        let next = offset + u32::from(instruction.size);
        self.state();

        let area = self.registers();
        let mut read: SmallVec<[Register; 8]> = SmallVec::new();
        instruction.for_each_use(body, |run| read.extend(run.registers()));
        for register in read {
            self.put(area, register, self.frame().get(register));
        }
        let start = self.word(code_block.instructions_ptr().addr() + offset as usize);
        let result = self.node(Kind::Interpret, &[area, start], instruction.opcode as u32);
        let code = self.ir().extract(result, 0);

        let has_profile = instruction
            .opcode
            .metadata()
            .is_some_and(|kind| kind.has_value_profile());
        let mut prediction = has_profile
            .then(|| self.prediction_at(instruction.metadata))
            .filter(|seen| !seen.is_empty());
        let mut written: SmallVec<[(Register, Written, Ssa); 4]> = SmallVec::new();
        instruction.for_each_def(body, |run, when| {
            written.extend(run.registers().map(|register| (register, when, Ssa::NONE)))
        });
        for (register, _, value) in &mut written {
            *value = self.take(area, *register);
            self.graph
                .predict(*value, prediction.take().unwrap_or(T::HEAP_TOP));
        }

        let flow = instruction.opcode.flow();
        let mut ways: SmallVec<[(usize, u32, EdgeKind); 4]> = SmallVec::new();
        let named_targets = instruction
            .opcode
            .shapes()
            .iter()
            .filter(|&&shape| shape == Shape::Target)
            .count();
        instruction.for_each_target(body, offset, |target| {
            let code = if ways.len() < named_targets {
                outcome::FIRST_TARGET + ways.len()
            } else {
                outcome::OFFSET + target as usize
            };
            if !ways.iter().any(|&(known, ..)| known == code) {
                ways.push((code, target, EdgeKind::Jump));
            }
        });
        if ways.is_empty() && flow.falls_through() {
            return written
                .into_iter()
                .for_each(|(register, _, value)| self.set(register, value));
        }
        if ways.is_empty() {
            debug_assert!(matches!(flow, Flow::Ends | Flow::Suspends));
            let value = self.ir().extract(result, 1);
            return self.leave_with(value);
        }
        if flow.falls_through() {
            ways.push((outcome::FALL_THROUGH, next, EdgeKind::FallThrough));
        }
        let before: SmallVec<[Ssa; 4]> = written
            .iter()
            .map(|&(register, ..)| self.frame().get(register))
            .collect();
        let (_, default, _) = *ways.last().unwrap();
        let cases: SmallVec<[(i64, u32); 4]> = ways[..ways.len() - 1]
            .iter()
            .map(|&(code, target, _)| (code as i64, target))
            .collect();
        self.switch_with(code, &cases, default, |builder, position| {
            for (&(register, when, value), &old) in written.iter().zip(&before) {
                builder.set(
                    register,
                    if when.holds_on(ways[position].2) {
                        value
                    } else {
                        old
                    },
                );
            }
        });
    }

    /// Where the interpreter is certain to throw.
    pub(super) fn exit_to_throw(&mut self) {
        if self.has_exited(ExitKind::WillThrow) {
            self.interpret();
        } else {
            self.force_exit(ExitKind::WillThrow);
        }
    }
}
