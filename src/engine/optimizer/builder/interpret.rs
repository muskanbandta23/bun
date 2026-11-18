//! Has the interpreter execute an instruction, on a copy of the registers that it names.

use bun_engine_asm::abi;
use bun_engine_bytecode::{EdgeKind, Flow, RawInstruction, Register, Shape, Written};
use bun_engine_codegen::ir::{Type, Value as Ssa};
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

    fn copy_out(&mut self, shadow: Ssa, register: Register, value: Ssa) {
        self.graph.shadow_frame.include(register);
        self.ir()
            .store(value, shadow, register.offset() * 8, heaps::FRAME);
    }

    pub(super) fn frame_pointer(&mut self) -> Ssa {
        self.graph.function.pinned(abi::FRAME_POINTER)
    }

    /// For the instructions that look at what the caller passed.
    pub(super) fn interpret_with_arguments(&mut self) {
        let ArgumentCount::Known(count) = self.frame().argument_count else {
            return self.interpret_on(Kind::InterpretWithArguments);
        };
        let shadow = self.insert_node(
            Kind::ShadowFrame,
            &[],
            0,
            bun_engine_codegen::ir::State::NONE,
        );
        let count = self.word(count as usize);
        self.copy_out(shadow, ARGUMENT_COUNT, count);
        for register in self.frame().argument_registers() {
            self.copy_out(shadow, register, self.frame().original_argument(register));
        }
        self.interpret();
    }

    /// A resumption passes the generator, the value and the mode.
    pub(super) fn interpret_resumption(&mut self) {
        let shadow = self.insert_node(
            Kind::ShadowFrame,
            &[],
            0,
            bun_engine_codegen::ir::State::NONE,
        );
        let frame_pointer = self.frame_pointer();
        for offset in ARGUMENT_COUNT.offset()..=Register::argument(1).offset() {
            let word = self
                .ir()
                .load(Type::I64, frame_pointer, offset * 8, heaps::FRAME);
            self.copy_out(shadow, Register::from_offset(offset), word);
        }
        self.interpret();
    }

    pub(super) fn interpret(&mut self) {
        self.interpret_on(Kind::Interpret);
    }

    fn interpret_on(&mut self, kind: Kind) {
        let frame = self.frame();
        let (code_block, offset) = (frame.code_block, frame.offset);
        let body = code_block.body();
        let instruction = RawInstruction::read(body.instructions(), offset).unwrap();
        let next = offset + u32::from(instruction.size);
        self.state();

        let shadow = self.insert_node(
            Kind::ShadowFrame,
            &[],
            0,
            bun_engine_codegen::ir::State::NONE,
        );
        let header = [
            (CODE_BLOCK, self.word(code_block.address())),
            (Register::CALLEE, self.get(Register::CALLEE)),
        ];
        let mut read: SmallVec<[(Register, Ssa); 8]> = header.into_iter().collect();
        instruction.for_each_use(body, |run| {
            read.extend(
                run.registers()
                    .map(|register| (register, self.frame().get(register))),
            )
        });
        for (register, value) in read {
            let value = self.graph.root(value);
            self.copy_out(shadow, register, value);
        }
        let start = self.word(code_block.instructions_ptr().addr() + offset as usize);
        let result = self.node(kind, &[shadow, start], instruction.opcode as u32);
        let code = self.ir().extract(result, 0);

        let mut prediction = match instruction.opcode.metadata() {
            Some(kind) if kind.has_value_profile() => {
                Some(self.prediction_at(instruction.metadata)).filter(|seen| !seen.is_empty())
            }
            _ => None,
        };
        let mut written: SmallVec<[(Register, Written, Ssa); 4]> = SmallVec::new();
        instruction.for_each_def(body, |run, when| {
            written.extend(run.registers().map(|register| (register, when, Ssa::NONE)))
        });
        for (register, _, value) in &mut written {
            self.graph.shadow_frame.include(*register);
            *value = self
                .ir()
                .load(Type::I64, shadow, register.offset() * 8, heaps::FRAME);
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
