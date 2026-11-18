//! Calls.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::{Operand, Register, Specialization};
use bun_engine_vm::code::metadata::{Call, SuperConstruct, WeakCell};
use bun_engine_vm::ic::CallProfile;
use bun_engine_vm::{CallFrame, Executable, JsFunction, options};

use super::{Cold, Compiler, metadata};
use crate::emit::{ARGUMENT_COUNT, WORD, at};
use crate::registers::*;

#[derive(Clone, Copy)]
pub(super) struct FixedCall {
    pub(super) dst: Register,
    pub(super) callee: Operand,
    pub(super) this: Operand,
    /// With `this`.
    pub(super) count: u32,
    pub(super) arguments: [Option<Operand>; 2],
    pub(super) frame: u32,
    pub(super) metadata: u32,
    pub(super) specialization: Specialization,
    pub(super) is_tail: bool,
    pub(super) is_super: bool,
}

/// Holds the entry while a call is being set up.
pub(crate) const ENTRY: Gpr = T4;
const MOST_UNROLLED: u32 = 8;

/// A field of the header of the frame that is `distance` words below.
pub(super) const fn new_frame(distance: u32, field: usize) -> Address {
    Address::new(FP, field as i32 - distance as i32 * WORD)
}

const fn frame_words(count_including_this: u32) -> u32 {
    (CallFrame::HEADER_WORDS as u32 + count_including_this).next_multiple_of(2)
}

impl Compiler<'_> {
    /// Fills the frame the general way, and leaves the entry and the executable in their registers.
    fn prepare_call(&mut self, cold: &Cold) {
        let opcode = self.raw(cold.offset).opcode;
        self.m.move32(ARGUMENTS[2], cold.offset);
        self.m.call_address(self.thunks.prepare_call(opcode));
        self.m.jump(cold.resume);
    }

    /// Where to go to have the call prepared, and where that comes back to.
    pub(super) fn cold_preparation(&mut self) -> (Label, Label) {
        let prepared = self.m.new_label();
        let prepare = self.cold_alone(prepared, |c, cold| c.prepare_call(cold));
        (prepare, prepared)
    }

    /// With the callee in `T0`. Goes on if it has an executable and the profile knows of it.
    pub(super) fn check_callee(&mut self, word: u32, otherwise: Label) {
        let known = self.m.new_label();
        let other = self.cold_with(known, otherwise, |c, cold| {
            let word = c.raw(cold.offset).metadata;
            (c.m).compute_address(T1, metadata(word, offset_of!(Call, profile)));
            c.m.call_address(c.thunks.check_callee);
            c.m.branch_if_not_zero32(T1, cold.resume);
            c.m.jump(cold.slow);
        });
        let seen = offset_of!(Call, profile) + CallProfile::OFFSET_OF_CALLEE;
        self.m.branch64(Cond::Ne, T0, metadata(word, seen), other);
        self.m.bind(known);
    }

    /// With the callee in `T0`, `this` in `T1` and the executable in its register.
    pub(super) fn fill_header(&mut self, frame: u32, count: u32, specialization: Specialization) {
        let code_block = Executable::offset_of_code_block(specialization);
        self.m.load64(T2, at(EXECUTABLE, code_block));
        let header = new_frame(frame, CallFrame::OFFSET_OF_CODE_BLOCK);
        self.m.store_pair64(header, T2, T0);
        (self.m).store64(new_frame(frame, CallFrame::OFFSET_OF_ARGUMENT_COUNT), count);
        (self.m).store64(new_frame(frame, CallFrame::OFFSET_OF_THIS), T1);
        let entry = Executable::offset_of_entry(specialization);
        self.m.load64(ENTRY, at(EXECUTABLE, entry));
        self.store_call_site();
    }

    pub(super) fn make_call(&mut self, frame: u32, dst: Register, word: u32) {
        (self.m).compute_address(SP, new_frame(frame, CallFrame::LINKAGE_SIZE));
        self.m.call_indirect(ENTRY);
        self.return_point();
        self.store_profiled_result(dst, word);
    }

    pub(super) fn fixed_call(&mut self, call: &FixedCall) {
        let (prepare, prepared) = self.cold_preparation();
        let moved = (0..).zip(call.arguments).filter_map(|(index, argument)| {
            let target = Register::outgoing_argument(call.frame, index);
            Some((target, argument.filter(|&it| it != target)?))
        });
        let temps = [T5, T6];
        for ((_, argument), temp) in moved.clone().zip(temps) {
            self.load(temp, argument);
        }
        self.load(T7, call.this);
        self.load(T0, call.callee);
        self.check_callee(call.metadata, prepare);
        if call.is_super {
            let known = self.m.new_label();
            let seen = metadata(call.metadata, offset_of!(SuperConstruct, new_target));
            self.m.branch64(Cond::Eq, T7, seen, known);
            self.m.branch64(Cond::Ne, seen, WeakCell::MANY, prepare);
            self.m.bind(known);
        }
        for ((target, _), temp) in moved.zip(temps) {
            self.store(target, temp);
        }
        self.m.move64(T1, T7);
        (self.m).load64(EXECUTABLE, at(T0, JsFunction::OFFSET_OF_EXECUTABLE));
        self.fill_header(call.frame, call.count, call.specialization);
        self.m.bind(prepared);
        if call.is_tail && options().use_tail_calls {
            return self.replace_frame(call.frame, call.count);
        }
        self.make_call(call.frame, call.dst, call.metadata);
    }

    /// Moves the new frame to where this one ends, and jumps to the entry.
    fn replace_frame(&mut self, frame: u32, count: u32) {
        if count > MOST_UNROLLED {
            self.m.compute_address(T1, new_frame(frame, 0));
            return self.m.jump_address(self.thunks.tail_call);
        }
        let parameters = self.body.parameters();
        self.m.load32(T0, ARGUMENT_COUNT);
        if parameters > 1 {
            self.m.move32(T1, parameters);
            self.m.select32(Cond::Below, T0, T0, T1, T1, T0);
        }
        self.m.add32(T0, T0, CallFrame::HEADER_WORDS + 1);
        self.m.and32(T0, T0, -2);
        let distance = -(frame_words(count) as i32) * WORD;
        (self.m).compute_address(T1, Address::indexed(FP, T0, Scale::Eight, distance));
        self.m.load_pair64(T5, T6, at(FP, 0));
        let first = CallFrame::LINKAGE_SIZE as u32 / WORD as u32;
        for word in (first..CallFrame::HEADER_WORDS as u32 + count).rev() {
            let offset = word as usize * WORD as usize;
            self.m.load64(T0, new_frame(frame, offset));
            self.m.store64(at(T1, offset), T0);
        }
        self.m.store_pair64(at(T1, 0), T5, T6);
        self.m.move64(FP, T1);
        self.m.leave_frame();
        self.m.jump_indirect(ENTRY);
    }

    pub(super) fn varargs_call(&mut self, dst: Register, word: u32, is_tail: bool) {
        let opcode = self.raw(self.offset).opcode;
        self.m.move32(ARGUMENTS[2], self.offset);
        self.m.call_address(self.thunks.size_frame(opcode));
        self.m.move64(SP, T0);
        self.m.move64(ARGUMENTS[3], T0);
        self.m.move32(ARGUMENTS[2], self.offset);
        self.m.call_address(self.thunks.fill_frame(opcode));
        if is_tail && options().use_tail_calls {
            self.m.move64(T1, SP);
            return self.m.jump_address(self.thunks.tail_call);
        }
        self.m.add64(SP, SP, CallFrame::LINKAGE_SIZE);
        self.m.call_indirect(ENTRY);
        self.return_point();
        self.store_profiled_result(dst, word);
    }
}
