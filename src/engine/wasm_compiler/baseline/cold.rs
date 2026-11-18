//! Code that is rarely run, which follows the rest of the function.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Reg};

use super::Function;
use super::exceptions::Handler;
use super::frame::CALLEE_WORDS_SIZE;
use super::registers::Local;
use crate::abi::{
    ARGUMENT_GPRS, CallProfile, FunctionSlot, INSTANCE, Operation, RESULT_GPRS, SLOT, Trap,
};

#[derive(Clone, Copy)]
pub(super) enum Cold {
    TierUpAtEntry {
        entry: Label,
        resume: Label,
    },
    TierUpInLoop {
        entry: Label,
        resume: Label,
        index: u32,
        snapshot: u32,
        dirty: RegisterSet,
    },
    TakeOverFrame {
        entry: Label,
        resume: Label,
        snapshot: u32,
        held: RegisterSet,
    },
    DivideByMinusOne {
        entry: Label,
        resume: Label,
        width: Width,
        dst: Gpr,
        lhs: Gpr,
    },
    MoveZero {
        entry: Label,
        resume: Label,
        dst: Gpr,
    },
    Operation {
        entry: Label,
        resume: Label,
        operation: Operation,
        immediate: Option<u32>,
        operand: Option<Gpr>,
        result: Option<Gpr>,
        live: RegisterSet,
    },
    NoteOtherCallee {
        entry: Label,
        resume: Label,
        profile: Gpr,
    },
    JumpThroughSlot {
        entry: Label,
        slot: usize,
    },
    ThrowAgain {
        entry: Label,
        exception: Gpr,
        handler: Handler,
    },
}

impl Function<'_> {
    #[inline]
    pub(super) fn trap_label(&mut self, trap: Trap) -> Label {
        *self.traps[trap as usize].get_or_insert_with(|| self.masm.new_label())
    }

    /// Control does not go on.
    pub(super) fn trap(&mut self, trap: Trap) {
        let label = self.trap_label(trap);
        self.masm.jump(label);
        self.is_dead = true;
    }

    pub(super) fn emit_cold_code(&mut self) {
        self.emit_detours();
        for index in 0..self.cold.len() {
            self.emit_cold(self.cold[index]);
        }
        for (label, &stub) in self.traps.iter().zip(&self.env.stubs.traps) {
            if let Some(label) = *label {
                self.masm.bind(label);
                self.masm.call_address(stub);
            }
        }
    }

    fn local_at(&self, snapshot: u32, reg: Reg) -> Local {
        self.locals[usize::from(self.snapshots[snapshot as usize][reg.index()])]
    }

    fn emit_cold(&mut self, cold: Cold) {
        match cold {
            Cold::TierUpAtEntry { entry, resume } => {
                self.masm.bind(entry);
                self.masm.call_address(self.env.stubs.tier_up_at_entry);
                self.masm.jump(resume);
            }
            Cold::TierUpInLoop {
                entry,
                resume,
                index,
                snapshot,
                dirty,
            } => {
                self.masm.bind(entry);
                self.masm.move32(SLOT, index);
                self.masm.call_address(self.env.stubs.tier_up_in_loop);
                self.masm.branch_if_zero64(SLOT, resume);
                for reg in dirty {
                    let Local { ty, offset, .. } = self.local_at(snapshot, reg);
                    self.store(ty.kind(), self.frame_address(offset), reg);
                }
                self.masm.jump_indirect(SLOT);
            }
            Cold::TakeOverFrame {
                entry,
                resume,
                snapshot,
                held,
            } => {
                self.masm.bind(entry);
                let bottom = Address::new(abi::FRAME_POINTER, -(self.frame.size() as i32));
                self.masm.compute_address(abi::STACK_POINTER, bottom);
                self.load_checked_memory();
                for reg in held {
                    let Local { ty, offset, .. } = self.local_at(snapshot, reg);
                    self.load(ty.kind(), reg, self.frame_address(offset));
                }
                self.masm.jump(resume);
            }
            Cold::DivideByMinusOne {
                entry,
                resume,
                width,
                dst,
                lhs,
            } => {
                let overflow = self.trap_label(Trap::IntegerOverflow);
                self.masm.bind(entry);
                (self.masm).branch_neg(width, ResultCond::Overflow, dst, lhs, overflow);
                self.masm.jump(resume);
            }
            Cold::MoveZero { entry, resume, dst } => {
                self.masm.bind(entry);
                self.masm.move32(dst, 0);
                self.masm.jump(resume);
            }
            Cold::Operation {
                entry,
                resume,
                operation,
                immediate,
                operand,
                result,
                live,
            } => {
                let vector_bytes = if self.uses_simd { 16 } else { 8 };
                let size = live.in_bank(Bank::Gpr).len() * 8
                    + live.in_bank(Bank::Fpr).len() * vector_bytes;
                let size = size.next_multiple_of(16) + CALLEE_WORDS_SIZE;
                let (stack_pointer, masm) = (abi::STACK_POINTER, &mut self.masm);
                let saved = Address::new(stack_pointer, CALLEE_WORDS_SIZE as i32);
                masm.bind(entry);
                masm.sub64(stack_pointer, stack_pointer, size);
                masm.store_registers(live, vector_bytes as i32, saved);
                let mut arguments = [Argument::Gpr(INSTANCE); 3];
                let mut count = 1;
                let given = [immediate.map(Argument::from), operand.map(Argument::from)];
                for argument in given.into_iter().flatten() {
                    arguments[count] = argument;
                    count += 1;
                }
                masm.move_arguments(&arguments[..count]);
                masm.call_address(self.env.stubs.operations[operation as usize]);
                if let Some(result) = result {
                    masm.move64(result, RESULT_GPRS[0]);
                }
                masm.load_registers(live, vector_bytes as i32, saved);
                masm.add64(stack_pointer, stack_pointer, size);
                masm.jump(resume);
            }
            Cold::NoteOtherCallee {
                entry,
                resume,
                profile,
            } => {
                let first = Address::new(profile, CallProfile::SLOT);
                let miss = self.masm.new_label();
                self.masm.bind(entry);
                self.masm.branch_if_not_zero64(first, miss);
                self.masm.store64(first, SLOT);
                self.masm.jump(resume);
                self.masm.bind(miss);
                (self.masm).add32_to_memory(Address::new(profile, CallProfile::MISSES), 1);
                self.masm.jump(resume);
            }
            Cold::JumpThroughSlot { entry, slot } => {
                self.masm.bind(entry);
                self.masm.move64(SLOT, slot);
                (self.masm).jump_indirect(Address::new(SLOT, FunctionSlot::ENTRY));
            }
            Cold::ThrowAgain {
                entry,
                exception,
                handler,
            } => {
                self.masm.bind(entry);
                self.masm.move64(ARGUMENT_GPRS[1], exception);
                self.masm.move64(ARGUMENT_GPRS[0], INSTANCE);
                let operation = self.env.stubs.operations[Operation::ThrowRef as usize];
                self.masm.call_address(operation);
                self.note_call_site(handler);
            }
        }
    }
}
