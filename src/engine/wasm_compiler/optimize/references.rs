//! References, structs and arrays.

use bun_engine_asm::{Sign, Size};
use bun_engine_codegen::ir::{
    Access, AccessFlags, Action, EffectFlags, Effects, Heap, Input, Lanes, PatchpointData, Type,
    Value,
};
use bun_engine_wasm_format::{
    AbstractHeapType, DataIndex, ElemIndex, FuncIndex, HeapType, RefType, StorageType, TypeIndex,
    ValType, VisitReference,
};

use super::emit::Sites;
use super::translator::{NONE, Operand, Translator, Visited, ir_type};
use crate::abi::{INT32_TAG, NULL, Operation, Trap};

/// What holds for a value wherever it is used.
#[derive(Clone, Copy)]
pub(super) enum Fact {
    Function(FuncIndex),
    /// Its type is not below the one with which it was pushed.
    NewStruct,
    NewArray {
        length: Value,
    },
}

#[derive(Default)]
pub(super) struct Facts(Vec<(Value, Fact)>);

impl Facts {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }

    /// `value` is newer than those before.
    pub(super) fn add(&mut self, value: Value, fact: Fact) {
        debug_assert!(self.0.last().is_none_or(|&(last, _)| last < value));
        self.0.push((value, fact));
    }

    pub(super) fn of(&self, value: Value) -> Option<Fact> {
        let position = self.0.binary_search_by_key(&value, |&(known, _)| known);
        position.ok().map(|position| self.0[position].1)
    }
}

/// A field or an element.
#[derive(Clone, Copy)]
struct Part {
    ty: StorageType,
    address: Value,
    offset: i32,
    access: Access,
}

impl Operand {
    pub(super) fn reference_type(self) -> RefType {
        self.ty.as_ref().expect("the code was validated")
    }

    pub(super) fn non_null(self) -> Self {
        let ty = self.reference_type().heap_type().non_null().into();
        Self { ty, ..self }
    }
}

impl Translator<'_> {
    pub(super) fn type_of_reference_to(&self, func: FuncIndex) -> ValType {
        let ty = self.module.func_type_index(func);
        self.types.heap_type(ty).non_null().into()
    }

    /// From here on the value has the type of the operand.
    pub(super) fn refine(&mut self, operand: Operand) {
        if operand.local != NONE {
            self.locals.refine(operand.local, operand.typed());
        }
    }

    pub(super) fn is_null(&mut self, operand: Operand) -> Value {
        if operand.reference_type().is_nullable() {
            self.b.equal(operand.value, NULL)
        } else {
            self.b.const32(0)
        }
    }

    pub(super) fn check_not_null(&mut self, operand: Operand, trap: Trap) {
        let is_null = self.is_null(operand);
        self.trap_if(is_null, trap);
        self.refine(operand.non_null());
    }

    /// The flags for an access to an object that starts at `offset`.
    pub(super) fn flags_for_access(&mut self, object: Operand, offset: i32) -> AccessFlags {
        let faults = ((NULL + offset) as u32) < self.env.host.null_guard_size;
        let may_be_null = object.reference_type().is_nullable();
        if may_be_null && faults && !self.b.op(object.value).is_constant() {
            self.refine(object.non_null());
            return AccessFlags::TRAPS;
        }
        self.check_not_null(object, Trap::NullReference);
        AccessFlags::empty()
    }

    /// `known`, or what the operation gives if that is zero.
    pub(super) fn or_make(&mut self, known: Value, operation: Operation, args: &[Value]) -> Value {
        let (done, result) = self.skip_with(known, known);
        let made = self.call_operation(operation, args, Type::I64, self.named.tables);
        self.b.jump(done, &[made]);
        self.b.switch_to(done);
        result
    }

    // The collector

    fn may_be_cell(&self, operand: Operand) -> bool {
        let is_cell_type = |ty: RefType| !ty.heap_type().is(AbstractHeapType::I31);
        operand.ty.as_ref().is_some_and(is_cell_type) && !self.b.op(operand.value).is_constant()
    }

    fn barrier_threshold(&mut self) -> Value {
        let address = (self.b).const64(self.env.host.barrier_threshold_address as i64);
        let access = Access::new(self.named.collector, AccessFlags::ALWAYS_READABLE);
        (self.b).load_narrow(Size::S8, Sign::Unsigned, address, 0, access)
    }

    /// Follows a store to the part of the cell that is in `heap`.
    pub(super) fn write_barrier(&mut self, cell: Value, stored: Operand, heap: Heap) {
        if !self.may_be_cell(stored) {
            return;
        }
        if cell == self.fresh_object {
            return self.finish_initialization();
        }
        // The collector may get to it from here.
        self.fresh_object = Value::NONE;
        let threshold = self.barrier_threshold();
        let mut effects = Effects::with_flags(EffectFlags::KEPT | EffectFlags::CONTROL_DEPENDENT);
        effects.reads = heap;
        let mut data = PatchpointData::new(Action::Custom, Sites::WRITE_BARRIER, effects);
        data.scratch_gprs = 1;
        let operands = [(cell, Input::LateRegister), (threshold, Input::Register)];
        self.b.patchpoint(&data, &operands, &[]);
    }

    /// Keeps the stores to a new object ahead of those that let the collector see it.
    fn finish_initialization(&mut self) {
        if cfg!(target_arch = "x86_64") {
            return self.b.fence(Heap::EVERYTHING, Heap::NOTHING);
        }
        let threshold = self.barrier_threshold();
        let mut effects = Effects::with_flags(EffectFlags::KEPT);
        effects.reads = Heap::EVERYTHING;
        let data = PatchpointData::new(Action::Custom, Sites::PUBLISH, effects);
        (self.b).patchpoint(&data, &[(threshold, Input::Register)], &[]);
    }

    fn push_new(&mut self, object: Value, ty: TypeIndex, fact: Fact) -> Visited {
        self.facts.add(object, fact);
        self.fresh_object = object;
        self.push(object, self.types.heap_type(ty).non_null());
        Ok(())
    }

    /// The object has just been made, with zeros and nulls.
    fn initialize(&mut self, values: &[Operand], part: impl Fn(&mut Self, u32) -> Part) {
        let mut has_cells = false;
        for (index, &value) in (0..).zip(values) {
            if self.default_value(value.ty).value != value.value {
                let part = part(self, index);
                self.store_part(part, value.value);
                has_cells |= self.may_be_cell(value);
            }
        }
        if has_cells {
            self.finish_initialization();
        }
    }

    // Fields and elements

    fn load_part(&mut self, part: Part, sign: Option<Sign>) -> Visited {
        let Part {
            ty,
            address,
            offset,
            access,
        } = part;
        let value = match ty.packed_size() {
            Some(size) => {
                let sign = sign.unwrap_or(Sign::Unsigned);
                self.b.load_narrow(size, sign, address, offset, access)
            }
            None => (self.b).load(ir_type(ty.unpacked()), address, offset, access),
        };
        self.push(value, ty.unpacked());
        Ok(())
    }

    fn store_part(&mut self, part: Part, value: Value) {
        match part.ty.packed_size() {
            Some(size) => (self.b).store_sized(size, value, part.address, part.offset, part.access),
            None => self.b.store(value, part.address, part.offset, part.access),
        }
    }

    /// The type of the object may be below `ty`, and say more about the field.
    fn field(&mut self, ty: TypeIndex, object: Operand, field: u32) -> Part {
        let ty = object.reference_type().heap_type().index().unwrap_or(ty);
        let data = self.types.struct_type(ty).fields[field as usize];
        let offset = self.env.host.struct_fields + data.offset as i32;
        let heap = self.heaps.field(self.b.function, self.types, ty, field);
        Part {
            ty: data.ty.storage,
            address: object.value,
            offset,
            access: Access::new(heap, self.flags_for_access(object, offset)),
        }
    }

    fn array_length(&mut self, array: Operand) -> Value {
        if let Some(Fact::NewArray { length }) = self.facts.of(array.value) {
            return length;
        }
        let offset = self.env.host.array_length;
        let flags = self.flags_for_access(array, offset) | AccessFlags::IMMUTABLE;
        let access = Access::new(Heap::NOTHING, flags);
        self.b.load(Type::I32, array.value, offset, access)
    }

    /// `index` is within bounds.
    fn element(&mut self, ty: TypeIndex, array: Value, index: Value) -> Part {
        let storage = self.types.array_type(ty).element.storage;
        let index = self.b.zero_extend(index);
        let distance = self.b.shl(index, i32::from(storage.log2_size()));
        Part {
            ty: storage,
            address: self.b.add(array, distance),
            offset: self.env.host.array_elements,
            access: (self.heaps.field(self.b.function, self.types, ty, 0)).into(),
        }
    }

    fn checked_element(&mut self, ty: TypeIndex, array: Operand, index: Value, trap: Trap) -> Part {
        let length = self.array_length(array);
        let is_outside = self.b.above_or_equal(index, length);
        self.trap_if(is_outside, trap);
        let ty = array.reference_type().heap_type().index().unwrap_or(ty);
        self.element(ty, array.value, index)
    }

    /// The low and the high half.
    fn bits_of(&mut self, value: Value) -> [Value; 2] {
        match self.b.type_of(value) {
            Type::V128 => [0, 1].map(|lane| {
                (self.b).extract_lane(Lanes::Int(Size::S64), Sign::Unsigned, value, lane)
            }),
            Type::F32 | Type::F64 => [self.b.bitcast(value), self.b.const64(0)],
            _ => [value, self.b.const64(0)],
        }
    }

    fn new_default_array(&mut self, ty: TypeIndex, length: Value) -> Value {
        let (args, writes) = ([self.b.const32(ty.0 as i32), length], self.named.collector);
        self.call_operation(Operation::ArrayNewDefault, &args, Type::I64, writes)
    }

    fn new_array_from_segment(
        &mut self,
        operation: Operation,
        ty: TypeIndex,
        segment: u32,
    ) -> Visited {
        let length = self.stack[self.stack.len() - 1].value;
        let (immediates, writes) = ([ty.0, segment], self.named.tables);
        let array = self.call_for_instruction::<2>(operation, &immediates, Type::I64, writes);
        self.push_new(array, ty, Fact::NewArray { length })
    }

    fn convert_reference(&mut self, value: Value, from: RefType, to: AbstractHeapType) -> Visited {
        self.push(value, RefType::new(HeapType::of(to), from.is_nullable()));
        Ok(())
    }
}

#[deny(clippy::missing_trait_methods)]
impl VisitReference for Translator<'_> {
    fn ref_null(&mut self, ty: HeapType) -> Visited {
        let null = self.default_value(ty.nullable().into());
        self.push(null.value, null.ty);
        Ok(())
    }

    fn ref_is_null(&mut self) -> Visited {
        let operand = self.pop();
        let result = self.is_null(operand);
        self.note_test(result, operand, operand.ty, operand.non_null().ty);
        self.push_comparison(result);
        Ok(())
    }

    fn ref_func(&mut self, func: FuncIndex) -> Visited {
        let offset = self.env.layout.function_reference(func);
        let heap = self.named.function_references;
        let known = self.load_readable(Type::I64, self.instance, offset, heap);
        let args = [self.b.const32(func.0 as i32)];
        let value = self.or_make(known, Operation::RefFunc, &args);
        self.facts.add(value, Fact::Function(func));
        self.push(value, self.type_of_reference_to(func));
        Ok(())
    }

    fn ref_eq(&mut self) -> Visited {
        let [lhs, rhs] = self.pop_values();
        let result = self.b.equal(lhs, rhs);
        self.push_comparison(result);
        Ok(())
    }

    fn ref_as_non_null(&mut self) -> Visited {
        let operand = self.pop();
        self.check_not_null(operand, Trap::NullAsNonNull);
        self.stack.push(operand.non_null());
        Ok(())
    }

    fn ref_test(&mut self, ty: RefType) -> Visited {
        self.test_type(ty);
        Ok(())
    }

    fn ref_cast(&mut self, ty: RefType) -> Visited {
        self.cast(ty);
        Ok(())
    }

    fn ref_i31(&mut self) -> Visited {
        let value = self.pop_value();
        let doubled = self.b.shl(value, 1);
        let extended = self.b.sar(doubled, 1);
        let wide = self.b.zero_extend(extended);
        let result = self.b.or(wide, INT32_TAG);
        self.push(result, HeapType::I31.non_null());
        Ok(())
    }

    fn i31_get(&mut self, sign: Sign) -> Visited {
        let operand = self.pop();
        self.check_not_null(operand, Trap::NullI31);
        let mut result = self.b.wrap(operand.value);
        if sign == Sign::Unsigned {
            result = self.b.and(result, i32::MAX);
        }
        self.push(result, ValType::I32);
        Ok(())
    }

    fn any_convert_extern(&mut self) -> Visited {
        let operand = self.pop();
        let tag = self.b.and(operand.value, INT32_TAG);
        let value = if self.b.is_int_constant(tag, 0) {
            operand.value
        } else {
            let is_no_number = self.b.is_zero(tag);
            let (done, result) = self.skip_with(is_no_number, operand.value);
            let (operation, args) = (Operation::AnyConvertExtern, [operand.value]);
            let converted = self.call_operation(operation, &args, Type::I64, Heap::NOTHING);
            self.b.jump(done, &[converted]);
            self.b.switch_to(done);
            result
        };
        self.convert_reference(value, operand.reference_type(), AbstractHeapType::Any)
    }

    fn extern_convert_any(&mut self) -> Visited {
        let operand = self.pop();
        let from = operand.reference_type();
        self.convert_reference(operand.value, from, AbstractHeapType::Extern)
    }

    fn struct_new(&mut self, ty: TypeIndex) -> Visited {
        let values = self.pop_many(self.types.struct_type(ty).fields.len());
        self.struct_new_default(ty)?;
        let object = self.stack[self.stack.len() - 1];
        self.initialize(&values, |this, index| this.field(ty, object, index));
        Ok(())
    }

    fn struct_new_default(&mut self, ty: TypeIndex) -> Visited {
        let (operation, writes) = (Operation::StructNew, self.named.collector);
        let object = self.call_for_instruction::<0>(operation, &[ty.0], Type::I64, writes);
        self.push_new(object, ty, Fact::NewStruct)
    }

    fn struct_get(&mut self, ty: TypeIndex, field: u32, sign: Option<Sign>) -> Visited {
        let object = self.pop();
        let part = self.field(ty, object, field);
        self.load_part(part, sign)
    }

    fn struct_set(&mut self, ty: TypeIndex, field: u32) -> Visited {
        let (value, object) = (self.pop(), self.pop());
        let part = self.field(ty, object, field);
        self.store_part(part, value.value);
        self.write_barrier(object.value, value, part.access.heap());
        Ok(())
    }

    fn array_new(&mut self, ty: TypeIndex) -> Visited {
        let (length, value) = (self.pop_value(), self.pop());
        let array = if self.default_value(value.ty).value == value.value {
            self.new_default_array(ty, length)
        } else {
            let [low, high] = self.bits_of(value.value);
            let args = [self.b.const32(ty.0 as i32), low, high, length];
            self.call_operation(Operation::ArrayNew, &args, Type::I64, self.named.collector)
        };
        self.push_new(array, ty, Fact::NewArray { length })
    }

    fn array_new_default(&mut self, ty: TypeIndex) -> Visited {
        let length = self.pop_value();
        let array = self.new_default_array(ty, length);
        self.push_new(array, ty, Fact::NewArray { length })
    }

    fn array_new_fixed(&mut self, ty: TypeIndex, length: u32) -> Visited {
        let values = self.pop_many(length as usize);
        let length = self.b.const32(length as i32);
        let array = self.new_default_array(ty, length);
        self.initialize(&values, |this, index| {
            let index = this.b.const32(index as i32);
            this.element(ty, array, index)
        });
        self.push_new(array, ty, Fact::NewArray { length })
    }

    fn array_new_data(&mut self, ty: TypeIndex, data: DataIndex) -> Visited {
        self.new_array_from_segment(Operation::ArrayNewData, ty, data.0)
    }

    fn array_new_elem(&mut self, ty: TypeIndex, elem: ElemIndex) -> Visited {
        self.new_array_from_segment(Operation::ArrayNewElem, ty, elem.0)
    }

    fn array_get(&mut self, ty: TypeIndex, sign: Option<Sign>) -> Visited {
        let (index, array) = (self.pop_value(), self.pop());
        let part = self.checked_element(ty, array, index, Trap::ArrayGetOutOfBounds);
        self.load_part(part, sign)
    }

    fn array_set(&mut self, ty: TypeIndex) -> Visited {
        let (value, index, array) = (self.pop(), self.pop_value(), self.pop());
        let part = self.checked_element(ty, array, index, Trap::ArraySetOutOfBounds);
        self.store_part(part, value.value);
        self.write_barrier(array.value, value, part.access.heap());
        Ok(())
    }

    fn array_len(&mut self) -> Visited {
        let array = self.pop();
        let length = self.array_length(array);
        self.push(length, ValType::I32);
        Ok(())
    }

    fn array_fill(&mut self, ty: TypeIndex) -> Visited {
        let [array, dst, value, count] = self.pop_values();
        let [low, high] = self.bits_of(value);
        let writes = self.heaps.field(self.b.function, self.types, ty, 0);
        let args = [array, dst, low, high, count];
        self.call_operation(Operation::ArrayFill, &args, Type::Void, writes);
        Ok(())
    }

    fn array_copy(&mut self, dst: TypeIndex, _: TypeIndex) -> Visited {
        let writes = self.heaps.field(self.b.function, self.types, dst, 0);
        self.call_for_instruction::<5>(Operation::ArrayCopy, &[], Type::Void, writes);
        Ok(())
    }

    fn array_init_data(&mut self, ty: TypeIndex, data: DataIndex) -> Visited {
        let writes = self.heaps.field(self.b.function, self.types, ty, 0);
        self.call_for_instruction::<4>(Operation::ArrayInitData, &[data.0], Type::Void, writes);
        Ok(())
    }

    fn array_init_elem(&mut self, _: TypeIndex, elem: ElemIndex) -> Visited {
        let writes = self.named.mutable;
        self.call_for_instruction::<4>(Operation::ArrayInitElem, &[elem.0], Type::Void, writes);
        Ok(())
    }
}
