use core::mem::offset_of;
use core::ops::Deref;

use bun_engine_bigint::{BigIntRef, Error, ErrorKind, Fill, New, Outcome, SmallBigInt};
use bun_engine_text::StringRef;

use crate::{
    CellKind, CellType, CellTypeRange, Class, ClassOps, Cx, ErrorType, Extends, Gc, GcFree, JsCell,
    JsResult, Shape, Thrown, TypeFlags, Visitor, Vm, ascii_literal,
};

/// The digits of the magnitude follow, the least significant first. The last is not zero.
#[repr(C)]
pub struct JsBigInt {
    base: JsCell,
    length: u32,
    is_negative: bool,
}

#[unsafe(export_name = "engine_class_BigInt")]
static BIGINT_CLASS: Class = Class::new::<JsBigInt>(
    ascii_literal!("BigInt"),
    None,
    CellTypeRange::new(CellType::BigInt, CellType::BigInt),
);

impl !GcFree for JsBigInt {}

impl Deref for JsBigInt {
    type Target = JsCell;

    #[inline(always)]
    fn deref(&self) -> &JsCell {
        &self.base
    }
}

// SAFETY: starts with a `JsCell`.
unsafe impl Extends<JsCell> for JsBigInt {}

pub enum Never {}

// SAFETY: refers to no cell but its shape, which is a root.
unsafe impl CellKind for JsBigInt {
    type Base = JsCell;
    type Init = Never;

    const CLASS: &'static Class = &BIGINT_CLASS;
    const CELL_TYPE: CellType = CellType::BigInt;
    const FLAGS: TypeFlags = TypeFlags::SHAPE_IS_IMMORTAL;
    const HOLDS_CLASS: bool = false;
    const OPS: ClassOps = JsCell::OPS;

    #[inline(always)]
    fn is(cell: &JsCell) -> bool {
        cell.is_bigint()
    }

    fn shape_of(init: &Never) -> Gc<Shape> {
        match *init {}
    }

    unsafe fn initialize(_: *mut JsBigInt, init: Never) {
        match init {}
    }

    fn trace_fields(&self, _: &mut Visitor) {}
}

impl JsBigInt {
    pub const OFFSET_OF_LENGTH: usize = offset_of!(JsBigInt, length);
    pub const OFFSET_OF_SIGN: usize = offset_of!(JsBigInt, is_negative);
    pub const OFFSET_OF_DIGITS: usize = size_of::<JsBigInt>();

    /// `fill` returns how many of the digits count, and the sign.
    #[inline(always)]
    fn try_create(
        vm: &Vm,
        capacity: usize,
        fill: impl FnOnce(&mut [u64]) -> Result<(u32, bool), Error>,
    ) -> Result<Gc<JsBigInt>, Error> {
        let size = Self::OFFSET_OF_DIGITS + capacity * size_of::<u64>();
        let address = vm.heap.cells.try_allocate(&vm.heap, size);
        if address == 0 {
            return Err(Error::OutOfMemory);
        }
        let cell = core::ptr::with_exposed_provenance_mut::<JsBigInt>(address);
        // SAFETY: a new cell of that size, which is zero until the digits are there.
        unsafe {
            JsCell::initialize(cell.cast(), vm.shapes.bigint());
            (&raw mut (*cell).length).write(0);
            (&raw mut (*cell).is_negative).write(false);
            let digits = cell.add(1).cast::<u64>();
            digits.write_bytes(0, capacity);
            let (length, is_negative) = fill(core::slice::from_raw_parts_mut(digits, capacity))?;
            assert!(length as usize <= capacity);
            (*cell).length = length;
            (*cell).is_negative = is_negative;
            Ok(Gc::from_address(address))
        }
    }

    #[inline]
    pub fn from_small(vm: &Vm, value: impl Into<SmallBigInt>) -> Gc<JsBigInt> {
        let value = value.into();
        let created = Self::try_create(vm, 2, |digits| {
            digits.copy_from_slice(&value.padded_digits());
            Ok((value.length() as u32, value.is_negative()))
        });
        created.unwrap_or_else(|_| bun_alloc::out_of_memory())
    }

    pub fn try_copy_of(vm: &Vm, value: BigIntRef<'_>) -> Result<Gc<JsBigInt>, Error> {
        Self::try_create(vm, value.digits().len(), |digits| {
            digits.copy_from_slice(value.digits());
            Ok((digits.len() as u32, value.is_negative()))
        })
    }

    pub fn try_from_new(vm: &Vm, new: New<impl Fill>) -> Result<Gc<JsBigInt>, Error> {
        match new {
            New::Small(small) => Ok(Self::from_small(vm, small)),
            New::Large(fill) => Self::try_create(vm, fill.digit_count(), |digits| {
                let filled = fill.fill(digits, &mut || vm.is_termination_requested())?;
                Ok((filled.length, filled.is_negative))
            }),
        }
    }

    #[inline]
    pub fn from_new(cx: &Cx, new: Result<New<impl Fill>, Error>) -> JsResult<Gc<JsBigInt>> {
        new.and_then(|new| Self::try_from_new(cx, new))
            .map_err(|error| Self::throw(cx, error))
    }

    /// The result of an operation on `first` and `second`.
    #[inline]
    pub fn from_outcome(
        cx: &Cx,
        first: &JsBigInt,
        second: &JsBigInt,
        outcome: Result<Outcome<impl Fill>, Error>,
    ) -> JsResult<Gc<JsBigInt>> {
        match outcome {
            Ok(Outcome::First) => Ok(first.into()),
            Ok(Outcome::Second) => Ok(second.into()),
            Ok(Outcome::New(new)) => Self::from_new(cx, Ok(new)),
            Err(error) => Err(Self::throw(cx, error)),
        }
    }

    #[cold]
    pub fn throw(cx: &Cx, error: Error) -> Thrown {
        match error.kind() {
            ErrorKind::RangeError => cx.throw_range_error(error.message()),
            ErrorKind::SyntaxError => cx.throw_error(ErrorType::SyntaxError, error.message()),
            ErrorKind::OutOfMemoryError => cx.throw_out_of_memory_with(error.message()),
            ErrorKind::Interruption => cx.throw_termination(),
        }
    }

    #[inline(always)]
    pub fn value(&self) -> BigIntRef<'_> {
        let digits = core::ptr::with_exposed_provenance(self.address() + Self::OFFSET_OF_DIGITS);
        // SAFETY: inside the cell, initialized, and never changed.
        let digits = unsafe { core::slice::from_raw_parts(digits, self.length as usize) };
        BigIntRef::new(self.is_negative, digits)
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.length == 0
    }

    pub fn to_string_ref(&self, cx: &Cx, radix: u32) -> JsResult<StringRef> {
        self.value()
            .to_string_ref(radix, &mut || cx.is_termination_requested())
            .map_err(|error| Self::throw(cx, error))
    }
}
