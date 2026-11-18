//! Indexed properties, by what the profiles have seen.

use bun_engine_bytecode::{MetadataKind, Operand, op};
use bun_engine_codegen::ir::{Access, AccessFlags, Heap, Type, Value as Ssa};
use bun_engine_vm::code::metadata::{ArrayAccess, GetByVal};
use bun_engine_vm::{ArrayModes, ElementKind, SpeculatedType as T};

use super::Builder;
use crate::graph::{ElementCheck, ExitKind, Kind, heaps};

const KINDS: ArrayModes = ArrayModes::NO_ELEMENTS
    .union(ArrayModes::UNDECIDED)
    .union(ArrayModes::INT32)
    .union(ArrayModes::DOUBLE)
    .union(ArrayModes::CONTIGUOUS)
    .union(ArrayModes::SPARSE)
    .union(ArrayModes::TYPED_ARRAYS)
    .union(ArrayModes::STRING)
    .union(ArrayModes::DIRECT_ARGUMENTS)
    .union(ArrayModes::SCOPED_ARGUMENTS)
    .union(ArrayModes::EXOTIC);

pub(super) fn single_kind(modes: ArrayModes) -> Option<ElementKind> {
    match modes & KINDS {
        ArrayModes::INT32 => Some(ElementKind::Int32),
        ArrayModes::DOUBLE => Some(ElementKind::Double),
        ArrayModes::CONTIGUOUS => Some(ElementKind::Contiguous),
        _ => None,
    }
}

fn arrayness(modes: ArrayModes) -> Option<bool> {
    (modes.contains(ArrayModes::ARRAY) != modes.contains(ArrayModes::NON_ARRAY))
        .then(|| modes.contains(ArrayModes::ARRAY))
}

fn heap_for(kind: ElementKind) -> Heap {
    if kind == ElementKind::Double {
        heaps::DOUBLE_ELEMENTS
    } else {
        heaps::VALUE_ELEMENTS
    }
}

impl Builder<'_> {
    fn may_speculate_on_elements(&self, key: Ssa) -> bool {
        let failures = [
            ExitKind::BadElements,
            ExitKind::BadType,
            ExitKind::NotAnInteger,
            ExitKind::OutOfBounds,
        ];
        T::BOXED_NUMBER.contains(self.expected(key))
            && !failures.into_iter().any(|kind| self.has_exited(kind))
    }

    fn element_address(&mut self, storage: Ssa, index: Ssa) -> Ssa {
        let mut ir = self.ir();
        let index = ir.zero_extend(index);
        let offset = ir.shl(index, 3);
        ir.add(storage, offset)
    }

    fn check_bounds(&mut self, index: Ssa, length: Ssa) {
        let is_outside = self.ir().above_or_equal(index, length);
        self.exit_if(is_outside, ExitKind::OutOfBounds);
    }

    pub(super) fn get_by_val(&mut self, op: op::GetByVal) {
        let code_block = self.frame().code_block;
        let modes = code_block
            .checked_metadata::<GetByVal>(MetadataKind::GetByVal, op.metadata)
            .array
            .modes();
        let Some(prediction) = self.prediction_or_exit(op.metadata) else {
            return;
        };
        let (base, key) = (self.get(op.base), self.get(op.key));
        let irregular = ArrayModes::OUT_OF_BOUNDS | ArrayModes::READ_HOLE;
        if !self.may_speculate_on_elements(key)
            || modes.intersects(irregular)
            || self.has_exited(ExitKind::LoadFromHole)
        {
            return self.interpret();
        }
        if modes & KINDS == ArrayModes::STRING {
            let unit = self.unit_at(base, key);
            let result = self.node(Kind::StringFromUnit, &[unit], 0);
            return self.set(op.dst, result);
        }
        let Some(kind) = single_kind(modes) else {
            return self.interpret();
        };
        let object = self.check_elements(base, ElementCheck::dense(kind, arrayness(modes), false));
        let index = self.as_int32(key);
        let storage = self.storage(object);
        let length = self.public_length(storage);
        self.check_bounds(index, length);
        let (address, access) = (
            self.element_address(storage, index),
            Access::new(heap_for(kind), AccessFlags::empty()),
        );
        let result = if kind == ElementKind::Double {
            let number = self.ir().load(Type::F64, address, 0, access);
            let is_hole =
                self.ir()
                    .float_compare(bun_engine_asm::FloatCond::Unordered, number, number);
            self.exit_if(is_hole, ExitKind::LoadFromHole);
            self.profiled_node(Kind::BoxDouble, &[number], 0, T::REAL_DOUBLE)
        } else {
            let value = self.ir().load(Type::I64, address, 0, access);
            let is_hole = self.ir().is_zero(value);
            self.exit_if(is_hole, ExitKind::LoadFromHole);
            if kind == ElementKind::Int32 {
                let int32 = self.ir().wrap(value);
                self.box_int32(int32)
            } else {
                self.graph.predict(value, prediction);
                self.knowledge
                    .learn_type(&self.graph, value, T::HEAP_TOP, value);
                value
            }
        };
        self.set(op.dst, result);
    }

    pub(super) fn unit_at(&mut self, string: Ssa, index: Ssa) -> Ssa {
        let string = self.check_type(string, T::STRING);
        let index = self.as_int32(index);
        let length = self.node(Kind::StringLength, &[string], 0);
        self.check_bounds(index, length);
        let string = self.resolve_rope(string);
        self.node(Kind::StringUnitAt, &[string, index], 0)
    }

    fn resolve_rope(&mut self, string: Ssa) -> Ssa {
        let resolved = T::ATOM_STRING | T::RESOLVED_STRING;
        if self.is(string, resolved) {
            return string;
        }
        let alias = self.node(Kind::ResolveRope, &[string], 0);
        let known = self.knowledge.entry(&self.graph, self.graph.root(string));
        (known.ty, known.alias) = (resolved, alias);
        alias
    }

    pub(super) fn put_by_val(
        &mut self,
        base: Operand,
        key: Operand,
        value: Operand,
        metadata: u32,
    ) {
        let code_block = self.frame().code_block;
        let entry = code_block.checked_metadata::<ArrayAccess>(MetadataKind::ArrayAccess, metadata);
        let modes = entry.array.modes();
        if modes.is_empty() && !entry.cases.has_run() && !self.has_exited(ExitKind::NeverExecuted) {
            return self.force_exit(ExitKind::NeverExecuted);
        }
        let (base, key, value) = (self.get(base), self.get(key), self.get(value));
        let irregular = ArrayModes::COPY_ON_WRITE | ArrayModes::SLOW_PUT;
        let fits = |kind: &ElementKind| match kind {
            ElementKind::Int32 => T::INT32.contains(self.expected(value)),
            ElementKind::Double => T::BOXED_NUMBER
                .difference(T::PURE_NAN)
                .contains(self.expected(value)),
            _ => true,
        };
        let is_regular = self.may_speculate_on_elements(key) && !modes.intersects(irregular);
        let Some(kind) = single_kind(modes).filter(fits).filter(|_| is_regular) else {
            return self.interpret();
        };
        let object = self.check_elements(base, ElementCheck::dense(kind, arrayness(modes), true));
        let index = self.as_int32(key);
        let stored = match kind {
            ElementKind::Int32 => {
                let int32 = self.as_int32(value);
                self.box_int32(int32)
            }
            ElementKind::Double => {
                let number = self.as_double(value);
                let is_nan =
                    self.ir()
                        .float_compare(bun_engine_asm::FloatCond::Unordered, number, number);
                self.exit_if(is_nan, ExitKind::BadType);
                number
            }
            _ => self.graph.root(value),
        };
        let storage = self.storage(object);
        if modes.intersects(ArrayModes::OUT_OF_BOUNDS | ArrayModes::STORED_TO_HOLE) {
            let is_negative = self.ir().less_than(index, 0);
            self.exit_if(is_negative, ExitKind::OutOfBounds);
            self.node(
                Kind::StoreElementGrowing,
                &[object, storage, index, stored],
                kind as u32,
            );
        } else {
            let length = self.public_length(storage);
            self.check_bounds(index, length);
            let address = self.element_address(storage, index);
            self.knowledge.note_write(&self.graph, heap_for(kind));
            self.ir().store(stored, address, 0, heap_for(kind));
            self.note_effect();
        }
        if kind == ElementKind::Contiguous {
            self.barrier(object, stored);
        }
    }
}
