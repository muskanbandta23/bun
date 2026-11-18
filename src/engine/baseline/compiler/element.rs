//! Properties whose key is a value.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::{Operand, op};
use bun_engine_vm::ArrayProfile;
use bun_engine_vm::code::metadata::{ArrayAccess, GetByVal};

use super::{Compiler, metadata};
use crate::emit::shape_id;
use crate::ic::abi::{BASE, ENTRY, KEY, RESULT, VALUE};
use crate::ic::{Elements, load_element, store_element};
use crate::registers::*;

impl Compiler<'_> {
    fn check_index(&mut self, key: Operand, other: Label) {
        self.m.branch_if_not_cell(BASE, other);
        if self.int32_constant(key).is_none() {
            self.m.branch_if_not_int32(KEY, other);
        }
        self.m.sign_extend32_to_64(T3, KEY);
    }

    fn record_shape(&mut self, profile: Address) {
        self.m.load32(T4, shape_id(BASE));
        (self.m).store32(profile.plus(ArrayProfile::OFFSET_OF_LAST_SHAPE as i32), T4);
    }

    pub(super) fn get_by_val(&mut self, op: op::GetByVal) {
        let found = self.m.new_label();
        let other = self.cold_until(found, |c, cold| {
            let start = metadata(c.raw(cold.offset).metadata, 0);
            c.call_cases(c.thunks.entries.get_by_val, start, cold.resume);
            c.m.jump(cold.slow);
        });
        self.load(BASE, op.base);
        self.load(KEY, op.key);
        match Elements::likely(self.profiles.get_by_val(op.metadata)) {
            Some(kind) => {
                let slow = self.slow();
                self.check_index(op.key, other);
                load_element(&mut self.m, kind, other, slow);
                self.record_shape(metadata(op.metadata, offset_of!(GetByVal, array)));
            }
            None => self.m.jump(other),
        }
        self.m.bind(found);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn get_by_val_with_this(&mut self, op: op::GetByValWithThis) {
        let (slow, found) = (self.slow(), self.m.new_label());
        self.load(BASE, op.base);
        self.load(KEY, op.key);
        self.load(VALUE, op.this);
        let entry = self.thunks.entries.get_by_val_with_this;
        self.call_cases(entry, metadata(op.metadata, 0), found);
        self.m.jump(slow);
        self.m.bind(found);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn put_by_val(&mut self, base: Operand, key: Operand, value: Operand, word: u32) {
        let other = self.cold(|c, cold| {
            let start = metadata(c.raw(cold.offset).metadata, 0);
            c.call_cases(c.thunks.entries.put_by_val, start, cold.resume);
            c.m.jump(cold.slow);
        });
        self.load(BASE, base);
        self.load(KEY, key);
        self.load(VALUE, value);
        let kind = Elements::likely(self.profiles.array_access(word));
        let Some(kind) =
            kind.filter(|it| !matches!(it, Elements::String | Elements::DirectArguments))
        else {
            return self.m.jump(other);
        };
        let (slow, profile) = (self.slow(), metadata(word, offset_of!(ArrayAccess, array)));
        self.check_index(key, other);
        let modes = profile.plus(ArrayProfile::OFFSET_OF_MODES as i32);
        store_element(&mut self.m, kind, VALUE, modes, other, slow);
        self.record_shape(profile);
        if let Elements::Contiguous | Elements::Values = kind {
            self.barrier_for(value, VALUE);
        }
    }

    pub(super) fn in_by_val(&mut self, op: op::InByVal) {
        self.load(KEY, op.key);
        self.slow_unless_object(op.base);
        let slow = self.slow();
        self.m.move64(BASE, T0);
        (self.m).compute_address(ENTRY, metadata(op.metadata, offset_of!(ArrayAccess, cases)));
        self.m.call_address(self.thunks.entries.in_by_val);
        self.m.branch_if_zero64(RESULT, slow);
        self.store_result(op.dst);
        self.carries_boolean = true;
    }
}
