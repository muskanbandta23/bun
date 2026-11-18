use core::mem::offset_of;
use core::ops::Deref;

use bun_alloc::AllocError;

use crate::heap::{barrier, visit_children_of};
use crate::{
    CellKind, CellType, CellTypeRange, Class, ClassOps, ElementKind, Extends, Gc, GcFree, JsCell,
    Shape, Storage, TypeFlags, Value, Visitor, Vm, ascii_literal,
};

/// A list without holes that never changes. After the header it looks like a storage, so arrays can share it until
/// they are written to.
#[repr(C)]
pub struct ImmutableArray {
    base: JsCell,
    public_length: u32,
    vector_length: u32,
}

#[unsafe(export_name = "engine_class_ImmutableArray")]
static IMMUTABLE_ARRAY_CLASS: Class = Class::new::<ImmutableArray>(
    ascii_literal!("Cell Butterfly"),
    None,
    CellTypeRange::new(CellType::ImmutableArray, CellType::ImmutableArray),
);

impl !GcFree for ImmutableArray {}

impl Deref for ImmutableArray {
    type Target = JsCell;

    #[inline(always)]
    fn deref(&self) -> &JsCell {
        &self.base
    }
}

// SAFETY: starts with a `JsCell`.
unsafe impl Extends<JsCell> for ImmutableArray {}

// SAFETY: visits the elements.
unsafe impl CellKind for ImmutableArray {
    type Base = JsCell;
    type Init = super::final_object::Never;

    const CLASS: &'static Class = &IMMUTABLE_ARRAY_CLASS;
    const CELL_TYPE: CellType = CellType::ImmutableArray;
    const FLAGS: TypeFlags = TypeFlags::SHAPE_IS_IMMORTAL;
    const HOLDS_CLASS: bool = false;
    const OPS: ClassOps = {
        let mut ops = JsCell::OPS;
        ops.visit_children = visit_children_of::<ImmutableArray>;
        ops
    };

    #[inline(always)]
    fn is(cell: &JsCell) -> bool {
        cell.cell_type() == CellType::ImmutableArray
    }

    fn shape_of(init: &Self::Init) -> Gc<Shape> {
        match *init {}
    }

    unsafe fn initialize(_: *mut ImmutableArray, init: Self::Init) {
        match init {}
    }

    fn trace_fields(&self, visitor: &mut Visitor) {
        if self.kind() == ElementKind::Contiguous {
            // SAFETY: that many values follow the header.
            for field in unsafe { self.storage().values(self.public_length) } {
                crate::Trace::trace(field, visitor);
            }
        }
    }
}

impl ImmutableArray {
    pub const OFFSET_OF_LENGTH: usize = offset_of!(ImmutableArray, public_length);
    pub const OFFSET_OF_ELEMENTS: usize = size_of::<ImmutableArray>();

    /// There is always room for one element, so that the pointer to the elements is inside the cell.
    pub const fn allocation_size(length: u32) -> usize {
        Self::OFFSET_OF_ELEMENTS + if length == 0 { 1 } else { length as usize } * size_of::<u64>()
    }

    /// `element` gives the word for each index.
    #[inline(always)]
    fn try_create(
        vm: &Vm,
        kind: ElementKind,
        length: usize,
        mut element: impl FnMut(usize) -> u64,
    ) -> Result<Gc<ImmutableArray>, AllocError> {
        if length > Storage::MAX_VECTOR_LENGTH as usize {
            return Err(AllocError);
        }
        let size = Self::allocation_size(length as u32);
        let address = vm.heap.cells.try_allocate(&vm.heap, size);
        if address == 0 {
            return Err(AllocError);
        }
        let _defer = vm.heap.defer_collection();
        // SAFETY: a new cell of that size, which is complete before the next collection.
        unsafe {
            let array = Gc::<ImmutableArray>::from_address(address);
            JsCell::initialize(array.as_ptr().cast(), vm.shapes.immutable_array(kind));
            let storage = Storage::from_address(address + Self::OFFSET_OF_ELEMENTS);
            storage.set_lengths(length as u32, length as u32);
            for index in 0..length {
                storage.elements_ptr().add(index).write(element(index));
            }
            barrier::publish_fence();
            Ok(array)
        }
    }

    #[inline]
    pub fn try_from_values(vm: &Vm, values: &[Value]) -> Result<Gc<ImmutableArray>, AllocError> {
        Self::try_from_fn(vm, values.len(), |index| values[index])
    }

    /// No collection runs meanwhile.
    #[inline]
    pub fn try_from_fn(
        vm: &Vm,
        length: usize,
        mut element: impl FnMut(usize) -> Value,
    ) -> Result<Gc<ImmutableArray>, AllocError> {
        Self::try_create(vm, ElementKind::Contiguous, length, |index| {
            element(index).to_bits()
        })
    }

    pub fn try_from_int32s(vm: &Vm, values: &[i32]) -> Result<Gc<ImmutableArray>, AllocError> {
        Self::try_create(vm, ElementKind::Int32, values.len(), |index| {
            Value::int32(values[index]).to_bits()
        })
    }

    pub fn try_from_doubles(vm: &Vm, values: &[f64]) -> Result<Gc<ImmutableArray>, AllocError> {
        if values.iter().any(|value| value.is_nan()) {
            return Self::try_from_fn(vm, values.len(), |index| Value::double(values[index]));
        }
        Self::try_create(vm, ElementKind::Double, values.len(), |index| {
            values[index].to_bits()
        })
    }

    /// # Safety
    /// The storage of an object whose indexing mode is copy on write.
    #[inline(always)]
    pub unsafe fn from_storage(storage: Storage) -> Gc<ImmutableArray> {
        // SAFETY: the caller's.
        unsafe { Gc::from_address(storage.address() - Self::OFFSET_OF_ELEMENTS) }
    }

    #[inline(always)]
    pub fn storage(&self) -> Storage {
        Storage::from_address(self.address() + Self::OFFSET_OF_ELEMENTS)
    }

    #[inline(always)]
    pub fn kind(&self) -> ElementKind {
        self.indexing_mode().kind()
    }

    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.public_length
    }

    pub fn is_empty(&self) -> bool {
        self.public_length == 0
    }

    #[inline(always)]
    pub fn get(&self, index: u32) -> Option<Value> {
        if index >= self.public_length {
            return None;
        }
        // SAFETY: inside, and of that kind.
        unsafe {
            if self.kind() == ElementKind::Double {
                Some(Value::double(self.storage().double_at(index).get()))
            } else {
                self.storage().value_at(index).get()
            }
        }
    }

    /// Unless the elements are raw doubles.
    #[inline]
    pub fn values(&self) -> Option<&[Value]> {
        let elements = self.storage().elements_ptr().cast();
        // SAFETY: none is empty, none changes, and the slice is inside the cell.
        (self.kind() != ElementKind::Double)
            .then(|| unsafe { core::slice::from_raw_parts(elements, self.len() as usize) })
    }

    pub fn iter(&self) -> impl Iterator<Item = Value> {
        (0..self.len()).filter_map(|index| self.get(index))
    }
}
