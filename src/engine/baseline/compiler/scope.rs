//! Variables that are not in the frame.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::{Instruction, Operand, Register, op};
use bun_engine_vm::code::metadata::{GetGlobal, GetImport, GlobalAccess};
use bun_engine_vm::{Environment, GlobalBinding, InlineWatchpointSet};

use super::{Compiler, metadata};
use crate::emit::{Emit, at, shape_id};
use crate::registers::*;

impl Compiler<'_> {
    fn load_scope(&mut self, dst: Gpr, scope: Register, depth: u32) {
        self.load(dst, scope);
        for _ in 0..depth {
            self.m.load64(dst, at(dst, Environment::OFFSET_OF_PARENT));
        }
    }

    pub(super) fn get_scoped(
        &mut self,
        dst: Register,
        (scope, depth, index): (Register, u32, u32),
        profile: Option<u32>,
        is_checked: bool,
    ) {
        self.load_scope(T0, scope, depth);
        (self.m).load64(T0, at(T0, Environment::allocation_size(index)));
        if is_checked {
            let slow = self.slow();
            self.m.branch_if_empty(T0, slow);
        }
        match profile {
            Some(word) => self.store_profiled_result(dst, word),
            None => self.store_result(dst),
        }
    }

    pub(super) fn put_scoped(
        &mut self,
        (scope, depth, index): (Register, u32, u32),
        value: Operand,
    ) {
        self.load_scope(T1, scope, depth);
        self.load(T0, value);
        (self.m).store64(at(T1, Environment::allocation_size(index)), T0);
        self.barrier_for(value, T0);
        self.keep(value);
    }

    pub(super) fn get_import(&mut self, op: op::GetImport) {
        let (slow, found) = (self.slow(), self.m.new_label());
        let varies = self.cold_until(found, |c, cold| {
            let Instruction::GetImport(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            c.m.branch_if_zero64(T0, cold.slow);
            c.load_scope(T1, op.scope, op.depth);
            (c.m).load_pair64(T0, T1, at(T1, Environment::allocation_size(op.slot)));
            c.m.branch_if_not_int32(T1, cold.slow);
            c.m.zero_extend32(T1, T1);
            let slots = Environment::OFFSET_OF_SLOTS as i32;
            (c.m).compute_address(T0, Address::indexed(T0, T1, Scale::Eight, slots));
            c.m.jump(cold.resume);
        });
        (self.m).load64(T0, metadata(op.metadata, offset_of!(GetImport, slot)));
        (self.m).branch64(Cond::BelowEq, T0, GetImport::VARIES, varies);
        self.m.bind(found);
        self.m.load64(T0, at(T0, 0));
        self.m.branch_if_empty(T0, slow);
        self.store_profiled_result(op.dst, op.metadata);
    }

    /// Leaves the address of the slot in `T2`, and the global object in `T1`.
    fn find_global_property(&mut self, access: Address, otherwise: Label) {
        let field = |offset: usize| access.plus(offset as i32);
        self.m.load32(T2, field(offset_of!(GlobalAccess, shape)));
        self.m.branch_if_zero32(T2, otherwise);
        self.m.load64(T1, field(offset_of!(GlobalAccess, pointer)));
        self.m.branch32(Cond::Ne, T2, shape_id(T1), otherwise);
        (self.m).load32_signed64(T3, field(offset_of!(GlobalAccess, byte_offset)));
        self.m.slot_base(T2, T1, T3);
        self.m.add64(T2, T2, T3);
    }

    /// Leaves its address in `dst`.
    fn find_global_binding(&mut self, dst: Gpr, access: Address, otherwise: Label) {
        let field = |offset: usize| access.plus(offset as i32);
        (self.m).branch_if_not_zero32(field(offset_of!(GlobalAccess, shape)), otherwise);
        self.m.load64(dst, field(offset_of!(GlobalAccess, pointer)));
        self.m.branch_if_zero64(dst, otherwise);
    }

    pub(super) fn get_global(&mut self, op: op::GetGlobal) {
        let access = metadata(op.metadata, offset_of!(GetGlobal, access));
        let (slow, found) = (self.slow(), self.m.new_label());
        if self.profiles.reads_global_property(op.metadata) {
            self.find_global_property(access, slow);
            self.m.load64(T0, at(T2, 0));
        } else {
            let property = self.cold_until(found, |c, cold| {
                let word = c.raw(cold.offset).metadata;
                c.find_global_property(metadata(word, offset_of!(GetGlobal, access)), cold.slow);
                c.m.load64(T0, at(T2, 0));
                c.m.jump(cold.resume);
            });
            self.find_global_binding(T0, access, property);
            self.m.load64(T0, at(T0, GlobalBinding::OFFSET_OF_VALUE));
        }
        self.m.bind(found);
        self.m.branch_if_empty(T0, slow);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn put_global(&mut self, op: op::PutGlobal) {
        let (access, slow) = (metadata(op.metadata, 0), self.slow());
        self.load(T0, op.value);
        if self.profiles.writes_global_property(op.metadata) {
            self.find_global_property(access, slow);
            self.m.store64(at(T2, 0), T0);
        } else {
            self.find_global_binding(T2, access, slow);
            let watchpoints = at(T2, GlobalBinding::OFFSET_OF_WATCHPOINTS);
            let invalidated = InlineWatchpointSet::THIN_INVALIDATED;
            self.m.branch64(Cond::Ne, watchpoints, invalidated, slow);
            self.m.store64(at(T2, GlobalBinding::OFFSET_OF_VALUE), T0);
            if !self.is_never_cell(op.value) {
                self.m.load_global(T1);
            }
        }
        self.barrier_for(op.value, T0);
        self.keep(op.value);
    }
}
