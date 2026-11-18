//! Properties that the instruction names.

use core::mem::offset_of;

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::{IdentifierIndex, Instruction, Operand, Register, op};
use bun_engine_text::{names, symbols};
use bun_engine_vm::code::metadata::{
    CasesAndValue, GetById, GetLength, GetPrivateName, PutPrivateName,
};
use bun_engine_vm::ic::{LoadCache, PrototypeLookup, StoreCache};
use bun_engine_vm::{
    ArrayModes, ArrayProfile, CellType, CellTypeRange, GlobalObject, IndexingMode, JsObject,
    MappedArguments, OFFSET_OF_INTERNAL_FIELDS, RealmSlot, Shape, TypeFlags,
};

use super::{Cold, Compiler, metadata};
use crate::emit::{Emit, FALSE, WORD, at, public_length, shape_id, storage};
use crate::ic::abi::{BASE, ENTRY, KEY, RESULT, VALUE};
use crate::profiles::{InlineLoad, InlineStore};
use crate::registers::*;

const LOAD_CACHE: usize = offset_of!(GetById, cache);
const _: () = assert!(LOAD_CACHE == offset_of!(GetLength, cache));
const _: () = assert!(LOAD_CACHE == offset_of!(GetPrivateName, cache));

impl Compiler<'_> {
    fn name_address(&self, name: IdentifierIndex) -> usize {
        self.code.identifiers[name.index()].name().address()
    }

    /// Goes to `handled` unless no case applied.
    pub(super) fn call_cases(&mut self, entry: usize, start: Address, handled: Label) {
        self.m.compute_address(ENTRY, start);
        self.store_call_site();
        self.m.call_address(entry);
        self.return_point();
        self.m.branch_if_not_zero64(RESULT, handled);
    }

    /// The same for an entry that runs no other code.
    fn call_plain_cases(&mut self, entry: usize, start: Address, handled: Label) {
        self.m.compute_address(ENTRY, start);
        self.m.call_address(entry);
        self.m.branch_if_not_zero64(RESULT, handled);
    }

    fn load_through_cases(&mut self, cold: &Cold) {
        let (name, word) = match self.instruction(cold.offset) {
            Instruction::GetById(op) => (self.name_address(op.name), op.metadata),
            Instruction::GetByIdDirect(op) => (self.name_address(op.name), op.metadata),
            Instruction::GetHasInstance(op) => (symbols::hasInstance.address(), op.metadata),
            Instruction::GetLength(op) => (names::length.address(), op.metadata),
            _ => unreachable!(),
        };
        self.m.move64(KEY, name);
        let entry = self.thunks.entries.get_by_id;
        self.call_cases(entry, metadata(word, LOAD_CACHE), cold.resume);
        self.m.jump(cold.slow);
    }

    pub(super) fn get_by_id(&mut self, dst: Register, base: Operand, word: u32) {
        let cache = |field: usize| metadata(word, LOAD_CACHE + field);
        let found = self.m.new_label();
        let other = self.cold_until(found, |c, cold| c.load_through_cases(cold));
        self.load(BASE, base);
        let inline = self.profiles.get_by_id(word);
        if inline == InlineLoad::None {
            self.m.jump(other);
        } else {
            self.m.branch_if_not_cell(BASE, other);
            self.m.load32(T0, shape_id(BASE));
            (self.m).branch32(Cond::Ne, T0, cache(LoadCache::OFFSET_OF_SHAPE), other);
        }
        let offset = cache(LoadCache::OFFSET_OF_BYTE_OFFSET);
        match inline {
            InlineLoad::None => {}
            InlineLoad::Constant => {
                self.m.branch_if_not_zero32(offset, other);
                (self.m).load64(T0, cache(LoadCache::OFFSET_OF_CASES_OR_LOOKUP));
                (self.m).load64(T0, at(T0, PrototypeLookup::OFFSET_OF_CONSTANT));
                self.m.branch_if_empty(T0, other);
            }
            InlineLoad::InObject => {
                self.m.load32_signed64(T0, offset);
                self.m.branch32(Cond::Le, T0, 0, other);
                (self.m).load64(T0, Address::indexed(BASE, T0, Scale::One, 0));
            }
            InlineLoad::InStorage => {
                self.m.load32_signed64(T0, offset);
                self.m.branch32(Cond::Ge, T0, 0, other);
                self.m.load64(T2, storage(BASE));
                self.m.load64(T0, Address::indexed(T2, T0, Scale::One, 0));
            }
        }
        self.m.bind(found);
        self.store_profiled_result(dst, word);
    }

    pub(super) fn get_has_instance(&mut self, op: op::GetHasInstance) {
        self.slow_unless_object(op.constructor);
        self.incoming = op.constructor.as_register();
        let uses_incoming = self.uses_incoming;
        self.get_by_id(op.dst, op.constructor, op.metadata);
        self.uses_incoming = uses_incoming;
    }

    pub(super) fn get_length(&mut self, op: op::GetLength) {
        let modes = self.profiles.get_length(op.metadata);
        let (found, slow) = (self.m.new_label(), self.slow());
        let other = self.cold_until(found, |c, cold| {
            c.m.call_address(c.thunks.get_length);
            c.m.branch_if_not_zero64(T0, cold.resume);
            c.load_through_cases(cold);
        });
        self.load(BASE, op.base);
        self.m.branch_if_not_cell(BASE, slow);
        self.m.load32(T0, shape_id(BASE));
        let last_shape = offset_of!(GetLength, array) + ArrayProfile::OFFSET_OF_LAST_SHAPE;
        self.m.store32(metadata(op.metadata, last_shape), T0);
        if modes.contains(ArrayModes::ARRAY) {
            load_array_length(&mut self.m, other);
        } else if modes.contains(ArrayModes::STRING) {
            self.m.branch_if_not_type(BASE, CellType::String, other);
            self.m.load_string_length(T0, BASE);
        } else {
            self.m.jump(other);
        }
        self.m.box_int32(T0, T0);
        self.m.bind(found);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn get_by_id_with_this(&mut self, op: op::GetByIdWithThis) {
        let slow = self.slow();
        self.load(BASE, op.base);
        self.load(VALUE, op.this);
        self.m.move64(KEY, self.name_address(op.name));
        let (found, entry) = (self.m.new_label(), self.thunks.entries.get_by_id_with_this);
        let cases = metadata(op.metadata, offset_of!(CasesAndValue, cases));
        self.call_cases(entry, cases, found);
        self.m.jump(slow);
        self.m.bind(found);
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn put_by_id(&mut self, op: op::PutById) {
        let cache = |field: usize| metadata(op.metadata, field);
        let other = self.cold(|c, cold| {
            let Instruction::PutById(op) = c.instruction(cold.offset) else {
                unreachable!()
            };
            c.m.move64(KEY, c.name_address(op.name));
            let entry = c.thunks.entries.put_by_id;
            c.call_cases(entry, metadata(op.metadata, 0), cold.resume);
            c.m.jump(cold.slow);
        });
        self.load(VALUE, op.value);
        self.load(BASE, op.base);
        let (is_transition, in_object) = match self.profiles.put_by_id(op.metadata) {
            InlineStore::None => return self.m.jump(other),
            InlineStore::Replace { in_object } => (false, in_object),
            InlineStore::Transition { in_object } => (true, in_object),
        };
        self.m.branch_if_not_cell(BASE, other);
        self.m.load32(T0, shape_id(BASE));
        (self.m).branch32(Cond::Ne, T0, cache(StoreCache::OFFSET_OF_SHAPE), other);
        let new_shape = cache(StoreCache::OFFSET_OF_NEW_SHAPE);
        if is_transition {
            self.m.load32(T3, new_shape);
            self.m.branch_if_zero32(T3, other);
            (self.m).load64(T0, cache(StoreCache::OFFSET_OF_CASES_OR_LOOKUP));
            let is_valid = at(T0, PrototypeLookup::OFFSET_OF_IS_VALID);
            self.m.branch8(Cond::Eq, is_valid, 0, other);
        } else {
            self.m.branch_if_not_zero32(new_shape, other);
        }
        (self.m).load32_signed64(T0, cache(StoreCache::OFFSET_OF_BYTE_OFFSET));
        if in_object {
            self.m.branch32(Cond::Le, T0, 0, other);
            (self.m).store64(Address::indexed(BASE, T0, Scale::One, 0), VALUE);
        } else {
            self.m.slot_base(T2, BASE, T0);
            (self.m).store64(Address::indexed(T2, T0, Scale::One, 0), VALUE);
        }
        if is_transition {
            self.m.store32(shape_id(BASE), T3);
            self.barrier();
        } else {
            self.barrier_for(op.value, VALUE);
        }
    }

    pub(super) fn in_by_id(&mut self, op: op::InById) {
        self.slow_unless_object(op.base);
        let (slow, found) = (self.slow(), self.m.new_label());
        self.m.move64(BASE, T0);
        self.m.move64(KEY, self.name_address(op.name));
        let entry = self.thunks.entries.in_by_id;
        self.call_plain_cases(entry, metadata(op.metadata, 0), found);
        self.m.jump(slow);
        self.m.bind(found);
        self.store_result(op.dst);
        self.carries_boolean = true;
    }

    pub(super) fn get_prototype_of(&mut self, op: op::GetPrototypeOf) {
        self.slow_unless_object(op.value);
        let slow = self.slow();
        (self.m).branch_if_flag(T0, TypeFlags::OVERRIDES_GET_PROTOTYPE, slow);
        self.m.load32(T0, shape_id(T0));
        self.m.decode_shape(T0, T0);
        self.m.load64(T0, at(T0, Shape::OFFSET_OF_PROTOTYPE));
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn instanceof_custom(&mut self, op: op::InstanceofCustom) {
        let (slow, done) = (self.slow(), self.target(op.done));
        self.load_both(op.has_instance.into(), op.constructor);
        self.m.load_global(T2);
        let default = GlobalObject::offset_of_slot(RealmSlot::function_prototype_has_instance);
        self.m.branch64(Cond::Ne, T0, at(T2, default), slow);
        (self.m).branch_unless_flag(T1, TypeFlags::IMPLEMENTS_DEFAULT_HAS_INSTANCE, slow);
        let (primitive, end) = (self.m.new_label(), self.end());
        self.load(T0, op.value);
        self.m.branch_if_not_cell(T0, primitive);
        self.m.branch_if_cell_is_object(T0, end);
        self.m.bind(primitive);
        self.store(op.dst, FALSE);
        self.m.jump(done);
    }

    pub(super) fn instanceof(&mut self, op: op::Instanceof) {
        let (slow, done) = (self.slow(), self.m.new_label());
        self.load(T1, op.prototype);
        self.slow_unless_object(op.value);
        self.m.branch_if_not_cell(T1, slow);
        self.m.branch_if_cell_is_no_object(T1, slow);
        let next = self.m.here();
        (self.m).branch_if_flag(T0, TypeFlags::OVERRIDES_GET_PROTOTYPE, slow);
        self.m.load32(T0, shape_id(T0));
        self.m.decode_shape(T0, T0);
        self.m.load64(T0, at(T0, Shape::OFFSET_OF_PROTOTYPE));
        self.m.branch64(Cond::Eq, T0, T1, done);
        self.m.branch_if_cell(T0, next);
        self.m.bind(done);
        self.m.compare64(Cond::Eq, T0, T0, T1);
        self.store_boolean_result(op.dst);
    }

    pub(super) fn get_internal_field(&mut self, op: op::GetInternalField) {
        self.load(T0, op.base);
        let offset = OFFSET_OF_INTERNAL_FIELDS + op.index as usize * WORD as usize;
        self.m.load64(T0, at(T0, offset));
        self.store_profiled_result(op.dst, op.metadata);
    }

    pub(super) fn put_internal_field(&mut self, op: op::PutInternalField) {
        let offset = OFFSET_OF_INTERNAL_FIELDS + op.index as usize * WORD as usize;
        self.store_in_cell(op.base, offset, op.value);
    }

    pub(super) fn has_shape_with_flags(&mut self, op: op::HasShapeWithFlags) {
        self.load(T0, op.object);
        self.m.load32(T0, shape_id(T0));
        self.m.decode_shape(T0, T0);
        (self.m).test32(
            TestCond::NonZero,
            T0,
            at(T0, Shape::OFFSET_OF_FLAGS),
            op.flags,
        );
        self.store_boolean_result(op.dst);
    }

    // Private names

    /// Goes on if the inline case is for the shape of the base and for the symbol.
    fn check_private_case(
        &mut self,
        base: Operand,
        symbol: Register,
        shape: Address,
        name: Address,
        other: Label,
    ) {
        let slow = self.slow();
        self.load(KEY, symbol);
        self.load(BASE, base);
        self.m.branch_if_not_cell(BASE, slow);
        self.m.load32(T0, shape_id(BASE));
        self.m.branch32(Cond::Ne, T0, shape, other);
        self.m.branch64(Cond::Ne, KEY, name, other);
    }

    pub(super) fn get_private_name(&mut self, op: op::GetPrivateName) {
        let cache = |field: usize| metadata(op.metadata, LOAD_CACHE + field);
        let found = self.m.new_label();
        let other = self.cold_until(found, |c, cold| {
            let cases = LOAD_CACHE + LoadCache::OFFSET_OF_CASES_OR_LOOKUP;
            let cases = metadata(c.raw(cold.offset).metadata, cases);
            c.call_plain_cases(c.thunks.entries.by_private_name, cases, cold.resume);
            c.m.jump(cold.slow);
        });
        let name = metadata(op.metadata, offset_of!(GetPrivateName, name));
        let shape = cache(LoadCache::OFFSET_OF_SHAPE);
        self.check_private_case(op.base, op.name, shape, name, other);
        (self.m).load32_signed64(T0, cache(LoadCache::OFFSET_OF_BYTE_OFFSET));
        self.m.slot_base(T2, BASE, T0);
        self.m.load64(T0, Address::indexed(T2, T0, Scale::One, 0));
        self.m.bind(found);
        self.store_profiled_result(op.dst, op.metadata);
    }

    fn private_store_case(&mut self, base: Operand, symbol: Register, word: u32) {
        let other = self.cold(|c, cold| {
            let cache = metadata(
                c.raw(cold.offset).metadata,
                offset_of!(PutPrivateName, cache),
            );
            let new_shape = cache.plus(StoreCache::OFFSET_OF_NEW_SHAPE as i32);
            c.m.branch_if_not_zero32(new_shape, cold.slow);
            let cases = cache.plus(StoreCache::OFFSET_OF_CASES_OR_LOOKUP as i32);
            c.call_plain_cases(c.thunks.entries.by_private_name, cases, cold.resume);
            c.m.jump(cold.slow);
        });
        let shape = offset_of!(PutPrivateName, cache) + StoreCache::OFFSET_OF_SHAPE;
        let name = metadata(word, offset_of!(PutPrivateName, name));
        self.check_private_case(base, symbol, metadata(word, shape), name, other);
    }

    pub(super) fn check_private_brand(&mut self, op: op::CheckPrivateBrand) {
        self.private_store_case(op.base, op.brand, op.metadata);
    }

    /// Without a value it sets a brand.
    pub(super) fn put_private_name(
        &mut self,
        base: Operand,
        symbol: Register,
        value: Option<Operand>,
        word: u32,
    ) {
        let cache = |field: usize| metadata(word, offset_of!(PutPrivateName, cache) + field);
        match value {
            Some(value) => self.load(VALUE, value),
            None => (self.m).move64(VALUE, JsObject::PRIVATE_BRAND_VALUE.to_bits()),
        }
        self.private_store_case(base, symbol, word);
        (self.m).load32_signed64(T0, cache(StoreCache::OFFSET_OF_BYTE_OFFSET));
        self.m.slot_base(T2, BASE, T0);
        (self.m).store64(Address::indexed(T2, T0, Scale::One, 0), VALUE);
        let same_shape = self.m.new_label();
        self.m.load32(T0, cache(StoreCache::OFFSET_OF_NEW_SHAPE));
        self.m.branch_if_zero32(T0, same_shape);
        self.m.store32(shape_id(BASE), T0);
        self.m.bind(same_shape);
        self.barrier();
    }
}

/// From `BASE` into `T0`, if it is an int32.
fn load_array_length(m: &mut MacroAssembler, otherwise: Label) {
    let mask = IndexingMode::IS_ARRAY | IndexingMode::KIND_MASK;
    m.load_indexing_mode(T0, BASE, mask);
    m.branch_test32(
        TestCond::Zero,
        T0,
        i32::from(IndexingMode::IS_ARRAY),
        otherwise,
    );
    m.branch32(Cond::Eq, T0, i32::from(IndexingMode::IS_ARRAY), otherwise);
    m.load64(T0, storage(BASE));
    m.load32(T0, public_length(T0));
    m.branch_test32(TestCond::Negative, T0, -1, otherwise);
}

/// The length of an array, a string or mapped arguments in `BASE`, or zero.
pub(crate) fn emit_get_length(m: &mut MacroAssembler) {
    let [no_array, no_string, miss, found] = core::array::from_fn(|_| m.new_label());
    load_array_length(m, no_array);
    m.jump(found);
    m.bind(no_array);
    m.branch_if_not_type(BASE, CellType::String, no_string);
    m.load_string_length(T0, BASE);
    m.jump(found);
    m.bind(no_string);
    let mapped = CellTypeRange::new(CellType::DirectArguments, CellType::ScopedArguments);
    m.branch_unless_type_in(BASE, mapped, T0, miss);
    m.branch_if_flag(BASE, TypeFlags::PER_CELL_BIT, miss);
    m.load32(T0, at(BASE, MappedArguments::OFFSET_OF_LENGTH));
    m.bind(found);
    m.box_int32(T0, T0);
    m.ret();
    m.bind(miss);
    m.move32(T0, 0);
    m.ret();
}
