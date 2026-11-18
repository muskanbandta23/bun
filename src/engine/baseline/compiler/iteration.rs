//! `for`-`in`, and iteration over arrays.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::{Instruction, Operand, Register, op};
use bun_engine_vm::code::metadata::{ForIn, ForInGet, IteratorNext};
use bun_engine_vm::{
    ArrayProfile, CellType, EnumerationMode, GlobalObject, ImmutableArray, IterationMode,
    IteratorRecord, JsObject, PropertyNameEnumerator, RealmSlot, RealmWatchpoint, Shape,
    ShapeFlags,
};

use super::{Compiler, metadata, slot};
use crate::emit::{Emit, TRUE, UNDEFINED, WORD, at, public_length, shape_id, storage};
use crate::ic::{Elements, load_element};
use crate::registers::*;

const OWN_SHAPE: i32 = EnumerationMode::OwnShape as i32;
const ENUMERATOR: Gpr = T2;
const INDEX: Gpr = T3;

const fn enumerator(field: usize) -> Address {
    at(ENUMERATOR, field)
}

impl Compiler<'_> {
    /// Goes on if `base`, which it leaves in `T1`, has the shape of the enumerator and a slot numbered `INDEX`, and
    /// the profile knows of the mode.
    fn check_own_slot(
        &mut self,
        base: Operand,
        state: Register,
        step: i32,
        modes: Address,
        otherwise: Label,
    ) {
        self.load(T1, base);
        self.m.load64(ENUMERATOR, slot(state));
        self.m.load32(INDEX, slot(state.nth(2)));
        if step != 0 {
            self.m.add32(INDEX, INDEX, step);
        }
        (self.m).branch32(Cond::Ne, slot(state.nth(1)), OWN_SHAPE, otherwise);
        self.m.branch_if_not_cell(T1, otherwise);
        self.m.load32(T0, shape_id(T1));
        let shape = enumerator(PropertyNameEnumerator::OFFSET_OF_SUBJECT_SHAPE);
        self.m.branch32(Cond::Ne, T0, shape, otherwise);
        let count = enumerator(PropertyNameEnumerator::OFFSET_OF_OWN_SHAPE_COUNT);
        self.m.branch32(Cond::AboveEq, INDEX, count, otherwise);
        (self.m).branch_test32(TestCond::Zero, modes, OWN_SHAPE, otherwise);
    }

    /// The address of that slot, in `T4`.
    fn find_own_slot(&mut self) {
        let (outside, found) = (self.m.new_label(), self.m.new_label());
        let capacity = enumerator(PropertyNameEnumerator::OFFSET_OF_INLINE_CAPACITY);
        self.m.load32(T4, capacity);
        self.m.branch32(Cond::AboveEq, INDEX, T4, outside);
        let first = size_of::<JsObject>() as i32;
        (self.m).compute_address(T4, Address::indexed(T1, INDEX, Scale::Eight, first));
        self.m.jump(found);
        self.m.bind(outside);
        self.m.sub64(T4, T4, INDEX);
        self.m.load64(T5, storage(T1));
        (self.m).compute_address(T4, Address::indexed(T5, T4, Scale::Eight, -2 * WORD));
        self.m.bind(found);
    }

    pub(super) fn for_in_next(&mut self, op: op::ForInNext) {
        let other = self.cold(|c, cold| {
            let Instruction::ForInNext(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            let (indexed, done) = (EnumerationMode::Indexed as i32, c.target(op.done));
            (c.m).branch32(Cond::Eq, slot(op.state.nth(1)), indexed, cold.slow);
            let count = enumerator(PropertyNameEnumerator::OFFSET_OF_NAME_COUNT);
            c.m.branch32(Cond::AboveEq, INDEX, count, done);
            c.m.jump(cold.slow);
        });
        let modes = metadata(op.metadata, offset_of!(ForIn, modes));
        self.check_own_slot(op.base.into(), op.state, 1, modes, other);
        self.m.store32(slot(op.state.nth(2)), INDEX);
        (self.m).load64(T0, enumerator(PropertyNameEnumerator::OFFSET_OF_NAMES));
        let first = ImmutableArray::OFFSET_OF_ELEMENTS as i32;
        (self.m).load64(T0, Address::indexed(T0, INDEX, Scale::Eight, first));
        self.store_result(op.name);
    }

    pub(super) fn for_in_get(&mut self, op: op::ForInGet) {
        let slow = self.slow();
        let modes = offset_of!(ForInGet, for_in) + offset_of!(ForIn, modes);
        self.check_own_slot(op.base, op.state, 0, metadata(op.metadata, modes), slow);
        self.find_own_slot();
        self.m.load64(T0, at(T4, 0));
        self.m.branch_if_empty(T0, slow);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn for_in_put(&mut self, op: op::ForInPut) {
        let slow = self.slow();
        self.load(T6, op.value);
        let modes = metadata(op.metadata, offset_of!(ForIn, modes));
        self.check_own_slot(op.base, op.state, 0, modes, slow);
        let is_writable = enumerator(PropertyNameEnumerator::OFFSET_OF_IS_WRITABLE);
        self.m.branch8(Cond::Eq, is_writable, 0, slow);
        self.m.decode_shape(T0, T0);
        let (flags, watched) = (
            at(T0, Shape::OFFSET_OF_FLAGS),
            ShapeFlags::IS_WATCHING_REPLACEMENT.bits(),
        );
        self.m
            .branch_test32(TestCond::NonZero, flags, watched, slow);
        self.find_own_slot();
        self.m.store64(at(T4, 0), T6);
        self.barrier_for(op.value, T6);
    }

    pub(super) fn for_in_has(&mut self, dst: Register, base: Operand, state: Register, word: u32) {
        let (slow, modes) = (self.slow(), metadata(word, offset_of!(ForIn, modes)));
        self.check_own_slot(base, state, 0, modes, slow);
        self.m.move32(T0, TRUE);
        self.store_result(dst);
        self.carries_boolean = true;
    }

    /// Loads the next element into `T0`. Goes to `no_array` with the `next` of the record in `T0`, and to `no_element`
    /// with the array in `T1` and the index in `INDEX`.
    fn step_array(&mut self, state: Register, word: u32, no_array: Label, no_element: Label) {
        let (slow, entry) = (self.slow(), |field: usize| metadata(word, field));
        let marker = IterationMode::Array.marker().to_bits();
        self.m.load64(T0, slot(state.nth(1)));
        self.m.branch64(Cond::Ne, T0, marker, no_array);
        let modes = entry(offset_of!(IteratorNext, modes));
        (self.m).branch_test32(TestCond::Zero, modes, IterationMode::Array.flag(), slow);
        self.m.load64(T1, slot(state));
        self.m.load32(INDEX, slot(state.nth(2)));
        let kind = match Elements::likely(self.profiles.iterated_array(word)) {
            Some(Elements::Double) => Elements::Double,
            _ => Elements::Values,
        };
        let other_kind = self.m.new_label();
        let found = self.m.new_label();
        load_element(&mut self.m, kind, other_kind, no_element);
        self.m.bind(found);
        let other = self.cold_with(found, slow, |c, cold| {
            let (values, miss) = (c.m.new_label(), cold.slow);
            load_element(&mut c.m, Elements::Double, values, miss);
            c.m.jump(cold.resume);
            c.m.bind(values);
            load_element(&mut c.m, Elements::Values, miss, miss);
            c.m.jump(cold.resume);
        });
        self.m.alias(other_kind, other);
        self.m.load32(T4, shape_id(T1));
        let last_shape = offset_of!(IteratorNext, array) + ArrayProfile::OFFSET_OF_LAST_SHAPE;
        self.m.store32(entry(last_shape), T4);
        self.m.add32(INDEX, INDEX, 1);
        self.m.store32(slot(state.nth(2)), INDEX);
    }

    /// Unless there is only a hole.
    fn mark_array_done(&mut self, state: Register, hole: Label) {
        self.m.load64(T4, storage(T1));
        self.m.branch32(Cond::Below, INDEX, public_length(T4), hole);
        self.store(state.nth(2), IteratorRecord::DONE.to_bits());
    }

    pub(super) fn iterator_next(&mut self, op: op::IteratorNext) {
        let no_array = self.cold(|c, cold| {
            let Instruction::IteratorNext(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            let markers = IterationMode::LAST_MARKER - IterationMode::FIRST_MARKER;
            c.m.sub64(T0, T0, IterationMode::FIRST_MARKER);
            c.m.branch64(Cond::BelowEq, T0, markers, cold.slow);
            let modes = metadata(op.metadata, offset_of!(IteratorNext, modes));
            let (flag, generic) = (IterationMode::Generic.flag(), c.target(op.generic));
            c.m.branch_test32(TestCond::NonZero, modes, flag, generic);
            c.m.jump(cold.slow);
        });
        let no_element = self.cold(|c, cold| {
            let Instruction::IteratorNext(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            c.mark_array_done(op.state, cold.slow);
            let done = c.target(op.done);
            c.m.jump(done);
        });
        self.step_array(op.state, op.metadata, no_array, no_element);
        self.store_profiled_result(op.value, op.metadata);
    }

    pub(super) fn iterator_step(&mut self, op: op::IteratorStep) {
        let (slow, store) = (self.slow(), self.m.new_label());
        let no_element = self.cold_until(store, |c, cold| {
            let Instruction::IteratorStep(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            c.mark_array_done(op.state, cold.slow);
            c.m.move32(T0, UNDEFINED);
            c.m.jump(cold.resume);
        });
        self.step_array(op.state, op.metadata, slow, no_element);
        self.m.bind(store);
        self.store_profiled_result(op.value, op.metadata);
    }

    pub(super) fn iterator_begin(&mut self, op: op::IteratorBegin) {
        let slow = self.slow();
        self.load(T1, op.iterable);
        let seen = metadata(op.metadata, 0);
        (self.m).branch_test32(TestCond::Zero, seen, IterationMode::Array.flag(), slow);
        self.m.call_address(self.thunks.is_pristine_array);
        self.m.branch_if_zero32(T0, slow);
        self.open_array(op.state);
    }

    /// The one in `T1`.
    fn open_array(&mut self, state: Register) {
        self.store(state, T1);
        self.store(state.nth(1), IterationMode::Array.marker().to_bits());
        self.store(state.nth(2), IteratorRecord::START.to_bits());
    }

    fn slow_unless_array_iteration_is_pristine(&mut self, global: Gpr) {
        let slow = self.slow();
        let protocol = GlobalObject::offset_of_watchpoints(RealmWatchpoint::ArrayIteratorProtocol);
        self.m.load64(global, at(global, protocol));
        self.m.branch_unless_watched(global, slow);
    }

    pub(super) fn iterator_open(&mut self, op: op::IteratorOpen) {
        let (slow, opened, seen) = (
            self.slow(),
            self.target(op.opened),
            metadata(op.metadata, 0),
        );
        self.load_both(op.method.into(), op.iterable);
        self.m.branch_if_not_cell(T1, slow);
        self.m.branch_if_not_type(T1, CellType::Array, slow);
        (self.m).branch_test32(TestCond::Zero, seen, IterationMode::Array.flag(), slow);
        self.m.load_global(T2);
        let values = GlobalObject::offset_of_slot(RealmSlot::array_prototype_values);
        self.m.branch64(Cond::Ne, T0, at(T2, values), slow);
        self.slow_unless_array_iteration_is_pristine(T2);
        self.open_array(op.state);
        self.m.jump(opened);
    }

    pub(super) fn iterator_close(&mut self, op: op::IteratorClose) {
        let (slow, end) = (self.slow(), self.end());
        self.m.branch32(Cond::Eq, slot(op.state.nth(2)), -1, end);
        let marker = IterationMode::Array.marker().to_bits();
        self.m
            .branch64(Cond::Ne, slot(op.state.nth(1)), marker, slow);
        let seen = metadata(op.metadata, 0);
        (self.m).branch_test32(TestCond::Zero, seen, IterationMode::Array.flag(), slow);
        self.m.load_global(T0);
        self.slow_unless_array_iteration_is_pristine(T0);
    }
}
