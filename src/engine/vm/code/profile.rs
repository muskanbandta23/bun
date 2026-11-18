//! What code that is not optimized records for the optimizer.

use core::cell::Cell;

use crate::{
    CellType, ElementKind, JsCell, JsObject, Shape, ShapeId, SpeculatedType, Value,
    heap::Survivors,
};

/// The last value that an instruction produced. It is not kept alive.
#[repr(transparent)]
pub struct ValueProfile(Cell<u64>);

impl ValueProfile {
    #[inline(always)]
    pub fn record(&self, value: Value) {
        self.0.set(value.to_bits());
    }

    #[inline(always)]
    pub fn record_bits(&self, bits: u64) {
        self.0.set(bits);
    }

    pub fn last(&self) -> Option<Value> {
        // SAFETY: cleared when a collection finds the cell dead, before it is swept.
        unsafe { Value::from_bits_or_empty(self.0.get()) }
    }

    pub(super) fn take_type(&self) -> SpeculatedType {
        let last = self.last();
        self.0.set(0);
        last.map_or_default(|value| SpeculatedType::of(Some(value)))
    }

    pub(super) fn clear_if_dead(&self, survivors: &Survivors) {
        if self.last().is_some_and(|it| !survivors.contains_value(it)) {
            self.0.set(0);
        }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    #[repr(transparent)]
    pub struct Arithmetic: u16 {
        const LHS_INT32 = 1 << 0;
        const LHS_DOUBLE = 1 << 1;
        const LHS_OTHER = 1 << 2;
        const RHS_INT32 = 1 << 3;
        const RHS_DOUBLE = 1 << 4;
        const RHS_OTHER = 1 << 5;
        /// Of operands that are int32.
        const RESULT_OVERFLOWED_INT32 = 1 << 6;
        const RESULT_OVERFLOWED_INT52 = 1 << 7;
        const RESULT_DOUBLE = 1 << 8;
        const RESULT_NEGATIVE_ZERO = 1 << 9;
        const RESULT_BIGINT = 1 << 10;
        const RESULT_STRING = 1 << 11;

        const BOTH_INT32 = Self::LHS_INT32.bits() | Self::RHS_INT32.bits();
    }
}

impl Arithmetic {
    #[inline(always)]
    pub fn of_lhs(value: Value) -> Arithmetic {
        if value.is_int32() {
            Arithmetic::LHS_INT32
        } else if value.is_number() {
            Arithmetic::LHS_DOUBLE
        } else {
            Arithmetic::LHS_OTHER
        }
    }

    #[inline(always)]
    pub fn of_rhs(value: Value) -> Arithmetic {
        Arithmetic::from_bits_retain(Self::of_lhs(value).bits() << 3)
    }

    pub fn of_result(value: Value) -> Arithmetic {
        const INT52_LIMIT: f64 = (1u64 << 51) as f64;
        match value.as_double() {
            Some(number) if number == 0.0 && number.is_sign_negative() => {
                Arithmetic::RESULT_NEGATIVE_ZERO
            }
            Some(number) if number.abs() >= INT52_LIMIT => {
                Arithmetic::RESULT_DOUBLE | Arithmetic::RESULT_OVERFLOWED_INT52
            }
            Some(_) => Arithmetic::RESULT_DOUBLE,
            None if value.is_bigint() => Arithmetic::RESULT_BIGINT,
            None if value.is_string() => Arithmetic::RESULT_STRING,
            None => Arithmetic::empty(),
        }
    }
}

/// An operator with one operand has it on the left.
#[repr(C, align(8))]
pub struct ArithmeticProfile(Cell<Arithmetic>);

impl ArithmeticProfile {
    #[inline(always)]
    pub fn get(&self) -> Arithmetic {
        self.0.get()
    }

    #[inline(always)]
    pub fn record(&self, seen: Arithmetic) {
        self.0.set(self.0.get() | seen);
    }

    pub fn record_unary(&self, operand: Value, result: Value) {
        let mut seen = Arithmetic::of_lhs(operand) | Arithmetic::of_result(result);
        if operand.is_int32() && !result.is_int32() {
            seen |= Arithmetic::RESULT_OVERFLOWED_INT32;
        }
        self.record(seen);
    }

    pub fn record_binary(&self, lhs: Value, rhs: Value, result: Value) {
        let mut seen =
            Arithmetic::of_lhs(lhs) | Arithmetic::of_rhs(rhs) | Arithmetic::of_result(result);
        if lhs.is_int32() && rhs.is_int32() && !result.is_int32() {
            seen |= Arithmetic::RESULT_OVERFLOWED_INT32;
        }
        self.record(seen);
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
    #[repr(transparent)]
    pub struct ArrayModes: u32 {
        const NO_ELEMENTS = 1 << 0;
        const UNDECIDED = 1 << 1;
        const INT32 = 1 << 2;
        const DOUBLE = 1 << 3;
        const CONTIGUOUS = 1 << 4;
        const SPARSE = 1 << 5;
        const ARRAY = 1 << 6;
        const NON_ARRAY = 1 << 7;
        const COPY_ON_WRITE = 1 << 8;
        const SLOW_PUT = 1 << 9;
        /// The first of twelve, in the order of the cell types.
        const INT8_ARRAY = 1 << 10;
        const TYPED_ARRAYS = 0xfff << 10;
        const STRING = 1 << 22;
        const DIRECT_ARGUMENTS = 1 << 23;
        const SCOPED_ARGUMENTS = 1 << 24;
        const EXOTIC = 1 << 25;
        const NON_ORIGINAL_ARRAY_SHAPE = 1 << 26;
        const RESIZABLE_TYPED_ARRAY = 1 << 27;
        const READ_HOLE = 1 << 28;
        /// Or appended.
        const STORED_TO_HOLE = 1 << 29;
        const OUT_OF_BOUNDS = 1 << 30;
        const LARGE_INDEX = 1 << 31;
    }
}

impl ArrayModes {
    pub fn of_shape(shape: &Shape) -> ArrayModes {
        let cell_type = shape.instance_type();
        if CellType::TYPED_ARRAYS.contains(cell_type) {
            let distance = cell_type.0 - CellType::Int8Array.0;
            return ArrayModes::from_bits_retain(Self::INT8_ARRAY.bits() << distance);
        }
        match cell_type {
            CellType::String => return ArrayModes::STRING,
            CellType::DirectArguments => return ArrayModes::DIRECT_ARGUMENTS,
            CellType::ScopedArguments => return ArrayModes::SCOPED_ARGUMENTS,
            _ if !cell_type.is_object() || shape.may_intercept_indexed_access() => {
                return ArrayModes::EXOTIC;
            }
            _ => {}
        }
        let mode = shape.instance_indexing_mode();
        let mut modes = ArrayModes::from_bits_retain(1 << (mode.kind() as u32 / 2));
        modes.set(ArrayModes::ARRAY, mode.is_array());
        modes.set(ArrayModes::NON_ARRAY, !mode.is_array());
        modes.set(ArrayModes::COPY_ON_WRITE, mode.is_copy_on_write());
        modes.set(ArrayModes::SLOW_PUT, mode.is_slow_put());
        let is_original = shape
            .global()
            .is_some_and(|global| global.is_original_array_shape(shape));
        modes.set(
            ArrayModes::NON_ORIGINAL_ARRAY_SHAPE,
            mode.is_array() && !is_original,
        );
        modes
    }
}

const _: () = assert!(1 << (ElementKind::Sparse as u32 / 2) == ArrayModes::SPARSE.bits());

#[repr(C, align(8))]
pub struct ArrayProfile {
    last_shape: Cell<ShapeId>,
    modes: Cell<ArrayModes>,
}

impl ArrayProfile {
    pub const OFFSET_OF_LAST_SHAPE: usize = core::mem::offset_of!(ArrayProfile, last_shape);
    pub const OFFSET_OF_MODES: usize = core::mem::offset_of!(ArrayProfile, modes);

    #[inline(always)]
    pub fn record_shape(&self, cell: &JsCell) {
        self.last_shape.set(cell.shape_id());
    }

    #[inline(always)]
    pub fn record(&self, seen: ArrayModes) {
        self.modes.set(self.modes.get() | seen);
    }

    /// For a slow path, which has the time not to lose what was seen before.
    pub fn record_base(&self, base: Value) {
        self.update();
        match base.as_cell() {
            Some(cell) => self.record_shape(&cell),
            None => self.record(ArrayModes::EXOTIC),
        }
    }

    /// For an access that the fast path did not take.
    pub fn record_index(&self, object: &JsObject, index: u32, is_store: bool) {
        if !object.element_kind().is_dense() {
            return;
        }
        let limit = if is_store {
            object.vector_length()
        } else {
            object.public_length()
        };
        if index >= limit {
            self.record(ArrayModes::OUT_OF_BOUNDS);
        } else if object.try_get_dense(index).is_none() {
            self.record(if is_store {
                ArrayModes::STORED_TO_HOLE
            } else {
                ArrayModes::READ_HOLE
            });
        }
    }

    pub(super) fn update(&self) {
        let shape = self.last_shape.replace(ShapeId::NONE);
        if !shape.is_none() {
            // SAFETY: updated when a collection ends, before anything is swept.
            self.record(ArrayModes::of_shape(&*unsafe { shape.decode() }));
        }
    }

    pub fn modes(&self) -> ArrayModes {
        self.update();
        self.modes.get()
    }
}
