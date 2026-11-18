//! References, structs and arrays.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Reg};
use bun_engine_wasm_format::{
    AbstractHeapType, DataIndex, ElemIndex, FuncIndex, HeapType, RefType, StorageType, TypeIndex,
    ValType, VisitReference,
};

use super::calls::Parameter;
use super::condition::Condition;
use super::operand::{Operand, bank};
use super::registers::Source;
use super::{Compiled, Function};
use crate::abi::{INSTANCE, INT32_TAG, NULL, Operation, Trap};

const I31_BITS: u8 = 31;

impl Function<'_> {
    pub(super) fn may_be_null(&self, value: Operand) -> bool {
        value.ty().as_ref().is_some_and(RefType::is_nullable)
    }

    pub(super) fn may_be_cell(&self, value: Operand) -> bool {
        let is_cell_type = |ty: RefType| !ty.heap_type().is(AbstractHeapType::I31);
        value.constant().is_none() && value.ty().as_ref().is_some_and(is_cell_type)
    }

    /// Of the operand at the top of the stack, which stays there.
    pub(super) fn top_gpr(&mut self) -> Gpr {
        match self.top() {
            value @ Operand::Local(..) => self.gpr(value),
            _ => self.own_top().gpr(),
        }
    }

    /// The last that an instruction does.
    pub(super) fn write_barrier(&mut self, cell: Gpr) {
        let host = self.env.host;
        let (state, threshold) = (self.take_gpr(), self.take_gpr());
        (self.masm).load8(threshold, Absolute(host.barrier_threshold_address));
        self.masm.load8(state, Address::new(cell, host.cell_state));
        let is_due = self.masm.new_label();
        (self.masm).branch32(Cond::BelowEq, state, threshold, is_due);
        self.call_operation_at(is_due, Operation::WriteBarrier, None, Some(cell), None);
    }

    fn finish_initialization(&mut self) {
        if cfg!(target_arch = "aarch64") {
            let threshold = Absolute(self.env.host.barrier_threshold_address);
            let done = self.masm.new_label();
            self.masm.branch_test8(TestCond::Zero, threshold, -1, done);
            self.masm.store_fence();
            self.masm.bind(done);
        }
    }

    /// For an access that starts at `offset`. `None` if it is null, and control does not go on.
    fn object(&mut self, value: Operand, offset: i32) -> Option<Gpr> {
        if value.constant().is_some() {
            self.trap(Trap::NullReference);
            return None;
        }
        let object = self.gpr(value);
        if self.may_be_null(value) && (NULL + offset) as u32 >= self.env.host.null_guard_size {
            let trap = self.trap_label(Trap::NullReference);
            self.masm.branch64(Cond::Eq, object, NULL, trap);
        }
        Some(object)
    }

    fn push_loaded(&mut self, ty: StorageType, sign: Option<Sign>, src: Address, reusable: Gpr) {
        let unpacked = ty.unpacked();
        let dst = match bank(unpacked.kind()) {
            Bank::Gpr => self.reuse(reusable),
            Bank::Fpr => self.take(Bank::Fpr),
        };
        match (ty.packed_size(), sign) {
            (Some(size), Some(Sign::Signed)) => {
                self.masm.load_signed(size, Width::W32, dst.gpr(), src);
            }
            (Some(size), _) => self.masm.load(size, dst.gpr(), src),
            (None, _) => self.load(unpacked.kind(), dst, src),
        }
        self.push_register(unpacked, dst);
    }

    /// Takes no register that the address could be in.
    fn store_field(&mut self, ty: StorageType, dst: Address, value: Operand) {
        match ty.packed_size() {
            Some(size) => {
                let value = self.reg_or_imm(value);
                self.masm.store(size, dst, value);
            }
            None => self.store_operand(dst, value),
        }
    }

    fn field_offset(&self, ty: TypeIndex, field: u32) -> (StorageType, i32) {
        let field = self.module.types().struct_type(ty).fields[field as usize];
        let offset = self.env.host.struct_fields + field.offset as i32;
        (field.ty.storage, offset)
    }

    fn element_type(&self, ty: TypeIndex) -> StorageType {
        self.module.types().array_type(ty).element.storage
    }

    fn push_new(&mut self, ty: TypeIndex) {
        self.push_operation_result(self.module.types().heap_type(ty).non_null().into());
    }

    /// Pops a value for each place in the object that was just made, the last first, and pushes the object.
    fn initialize(
        &mut self,
        ty: TypeIndex,
        places: impl Iterator<Item = (StorageType, i32)>,
    ) -> Compiled {
        let object = crate::abi::RESULT_GPRS[0];
        for (storage, offset) in places {
            self.registers.locked = RegisterSet::of(&[object], &[]);
            let value = self.pop();
            let default = if storage.is_ref() { NULL as u64 } else { 0 };
            if value.constant() != Some(default) {
                self.store_field(storage, Address::new(object, offset), value);
            }
        }
        self.finish_initialization();
        self.push_new(ty);
        self.check_size()
    }

    fn element_address(
        &mut self,
        ty: StorageType,
        array: Operand,
        index: Operand,
        out_of_bounds: Trap,
    ) -> Option<(Gpr, Address)> {
        let host = self.env.host;
        let array = self.object(array, host.array_length)?;
        let length = Address::new(array, host.array_length);
        let trap = self.trap_label(out_of_bounds);
        let near = index.constant().and_then(|index| {
            let offset = i32::try_from(index << ty.log2_size()).ok()?;
            Some((index, offset.checked_add(host.array_elements)?))
        });
        if let Some((index, offset)) = near {
            (self.masm).branch32(Cond::BelowEq, length, Untrusted(index), trap);
            return Some((array, Address::new(array, offset)));
        }
        let mut index = self.gpr(index);
        self.masm.branch32(Cond::AboveEq, index, length, trap);
        let scale = Scale::from_log2(ty.log2_size()).unwrap_or_else(|| {
            let doubled = self.reuse(index).gpr();
            self.masm.add64(doubled, index, index);
            index = doubled;
            Scale::Eight
        });
        let address = Address::indexed(array, index, scale, host.array_elements);
        Some((array, address))
    }

    fn check_not_null(&mut self, value: Operand, reg: impl Into<Reg>, trap: Trap) {
        if self.may_be_null(value) {
            let trap = self.trap_label(trap);
            self.masm.branch64(Cond::Eq, reg.into().gpr(), NULL, trap);
        }
    }

    fn convert_top(&mut self, to: HeapType) -> ValType {
        RefType::new(to, self.may_be_null(self.top())).into()
    }
}

#[deny(clippy::missing_trait_methods)]
impl VisitReference for Function<'_> {
    fn ref_null(&mut self, ty: HeapType) -> Compiled {
        self.push_lazy(Operand::Constant(ty.nullable().into(), NULL as u64));
        Ok(())
    }

    fn ref_is_null(&mut self) -> Compiled {
        let value = self.pop();
        if value.constant().is_some() || !self.may_be_null(value) {
            let is_null = value.constant() == Some(NULL as u64);
            self.push_lazy(Operand::int(Width::W32, is_null.into()));
            return Ok(());
        }
        let (value, null) = (self.source(value), Source::Constant(NULL as u64));
        let is_null = Condition::Compare(Width::W64, Cond::Eq, value, null);
        self.push_condition(is_null, self.next());
        Ok(())
    }

    fn ref_func(&mut self, func: FuncIndex) -> Compiled {
        let dst = self.take_gpr();
        let known = Address::new(INSTANCE, self.env.layout.function_reference(func));
        self.masm.load64(dst, known);
        let missing = self.masm.new_label();
        self.masm.branch_if_zero64(dst, missing);
        self.call_operation_at(missing, Operation::RefFunc, Some(func.0), None, Some(dst));
        let ty = self.module.func_type_index(func);
        self.push_register(self.module.types().heap_type(ty).non_null().into(), dst);
        Ok(())
    }

    fn ref_eq(&mut self) -> Compiled {
        self.compare_ints(Width::W64, Cond::Eq, self.next());
        Ok(())
    }

    fn ref_as_non_null(&mut self) -> Compiled {
        let value = self.top();
        if value.constant() == Some(NULL as u64) {
            self.trap(Trap::NullAsNonNull);
            return Ok(());
        }
        if self.may_be_null(value) {
            let reg = self.top_gpr();
            self.check_not_null(value, reg, Trap::NullAsNonNull);
        }
        let ty = value.ty().as_ref().expect("a reference");
        self.narrow_top(ty.heap_type().non_null());
        Ok(())
    }

    fn ref_test(&mut self, ty: RefType) -> Compiled {
        self.test_type(ty);
        Ok(())
    }

    fn ref_cast(&mut self, ty: RefType) -> Compiled {
        self.cast(ty);
        Ok(())
    }

    fn ref_i31(&mut self) -> Compiled {
        const I31REF: ValType = HeapType::I31.non_null().value_type();
        let value = self.pop();
        if let Some(bits) = value.constant() {
            let extended = ((bits as i32) << 1 >> 1) as u32;
            self.push_lazy(Operand::Constant(I31REF, INT32_TAG | u64::from(extended)));
            return Ok(());
        }
        let src = self.gpr(value);
        let dst = self.reuse(src).gpr();
        (self.masm).extract_bits(Sign::Signed, Width::W32, dst, src, 0, I31_BITS);
        self.masm.or64(dst, dst, INT32_TAG);
        self.push_register(I31REF, dst);
        Ok(())
    }

    fn i31_get(&mut self, sign: Sign) -> Compiled {
        let mask = match sign {
            Sign::Signed => u32::MAX,
            Sign::Unsigned => u32::MAX >> (32 - I31_BITS),
        };
        let value = self.pop();
        match value.constant() {
            Some(bits) if bits == NULL as u64 => self.trap(Trap::NullI31),
            Some(bits) => self.push_lazy(Operand::int(Width::W32, bits & u64::from(mask))),
            None => {
                let src = self.gpr(value);
                self.check_not_null(value, src, Trap::NullI31);
                let dst = self.reuse(src).gpr();
                self.masm.and32(dst, src, mask);
                self.push_register(ValType::I32, dst);
            }
        }
        Ok(())
    }

    fn any_convert_extern(&mut self) -> Compiled {
        let ty = self.convert_top(HeapType::ANY);
        if self.top().constant().is_some() {
            self.narrow_top(ty);
            return Ok(());
        }
        self.call_with_operands::<0, 1>(Operation::AnyConvertExtern, []);
        self.push_operation_result(ty);
        Ok(())
    }

    fn extern_convert_any(&mut self) -> Compiled {
        let ty = self.convert_top(HeapType::EXTERN);
        self.narrow_top(ty);
        Ok(())
    }

    fn struct_new(&mut self, ty: TypeIndex) -> Compiled {
        self.call_with_operands::<1, 0>(Operation::StructNew, [ty.0.into()]);
        let start = self.env.host.struct_fields;
        let fields = self.module.types().struct_type(ty).fields.iter().rev();
        let places = fields.map(|field| (field.ty.storage, start + field.offset as i32));
        self.initialize(ty, places)
    }

    fn struct_new_default(&mut self, ty: TypeIndex) -> Compiled {
        self.call_with_operands::<1, 0>(Operation::StructNew, [ty.0.into()]);
        self.push_new(ty);
        Ok(())
    }

    fn struct_get(&mut self, ty: TypeIndex, field: u32, sign: Option<Sign>) -> Compiled {
        let (storage, offset) = self.field_offset(ty, field);
        let object = self.pop();
        if let Some(object) = self.object(object, offset) {
            self.push_loaded(storage, sign, Address::new(object, offset), object);
        }
        Ok(())
    }

    fn struct_set(&mut self, ty: TypeIndex, field: u32) -> Compiled {
        let (storage, offset) = self.field_offset(ty, field);
        let (value, object) = (self.pop(), self.pop());
        self.lock(value);
        if let Some(object) = self.object(object, offset) {
            self.store_field(storage, Address::new(object, offset), value);
            if self.may_be_cell(value) {
                self.write_barrier(object);
            }
        }
        Ok(())
    }

    fn array_new(&mut self, ty: TypeIndex) -> Compiled {
        let length = Parameter::Value(self.pop());
        let [low, high] = self.pop_bits();
        let parameters = [Parameter::Immediate(ty.0.into()), low, high, length];
        self.call_operation(Operation::ArrayNew, &parameters);
        self.push_new(ty);
        Ok(())
    }

    fn array_new_default(&mut self, ty: TypeIndex) -> Compiled {
        self.call_with_operands::<1, 1>(Operation::ArrayNewDefault, [ty.0.into()]);
        self.push_new(ty);
        Ok(())
    }

    fn array_new_fixed(&mut self, ty: TypeIndex, length: u32) -> Compiled {
        let immediates = [ty.0.into(), length.into()];
        self.call_with_operands::<2, 0>(Operation::ArrayNewDefault, immediates);
        let (storage, start) = (self.element_type(ty), self.env.host.array_elements);
        let place = |index| (storage, start + (index << storage.log2_size()));
        self.initialize(ty, (0..length as i32).rev().map(place))
    }

    fn array_new_data(&mut self, ty: TypeIndex, data: DataIndex) -> Compiled {
        self.call_with_operands::<2, 2>(Operation::ArrayNewData, [ty.0.into(), data.0.into()]);
        self.push_new(ty);
        Ok(())
    }

    fn array_new_elem(&mut self, ty: TypeIndex, elem: ElemIndex) -> Compiled {
        self.call_with_operands::<2, 2>(Operation::ArrayNewElem, [ty.0.into(), elem.0.into()]);
        self.push_new(ty);
        Ok(())
    }

    fn array_get(&mut self, ty: TypeIndex, sign: Option<Sign>) -> Compiled {
        let storage = self.element_type(ty);
        let (index, array) = (self.pop(), self.pop());
        let trap = Trap::ArrayGetOutOfBounds;
        if let Some((array, address)) = self.element_address(storage, array, index, trap) {
            self.push_loaded(storage, sign, address, array);
        }
        Ok(())
    }

    fn array_set(&mut self, ty: TypeIndex) -> Compiled {
        let storage = self.element_type(ty);
        let (value, index, array) = (self.pop(), self.pop(), self.pop());
        self.lock(value);
        let trap = Trap::ArraySetOutOfBounds;
        if let Some((array, address)) = self.element_address(storage, array, index, trap) {
            self.store_field(storage, address, value);
            if self.may_be_cell(value) {
                self.write_barrier(array);
            }
        }
        Ok(())
    }

    fn array_len(&mut self) -> Compiled {
        let offset = self.env.host.array_length;
        let array = self.pop();
        if let Some(array) = self.object(array, offset) {
            let address = Address::new(array, offset);
            self.push_loaded(ValType::I32.into(), None, address, array);
        }
        Ok(())
    }

    fn array_fill(&mut self, _: TypeIndex) -> Compiled {
        let count = Parameter::Value(self.pop());
        let [low, high] = self.pop_bits();
        let (dst, array) = (Parameter::Value(self.pop()), Parameter::Value(self.pop()));
        self.call_operation(Operation::ArrayFill, &[array, dst, low, high, count]);
        Ok(())
    }

    fn array_copy(&mut self, _: TypeIndex, _: TypeIndex) -> Compiled {
        self.call_with_operands::<0, 5>(Operation::ArrayCopy, []);
        Ok(())
    }

    fn array_init_data(&mut self, _: TypeIndex, data: DataIndex) -> Compiled {
        self.call_with_operands::<1, 4>(Operation::ArrayInitData, [data.0.into()]);
        Ok(())
    }

    fn array_init_elem(&mut self, _: TypeIndex, elem: ElemIndex) -> Compiled {
        self.call_with_operands::<1, 4>(Operation::ArrayInitElem, [elem.0.into()]);
        Ok(())
    }
}
