use core::ops::Deref;

use crate::heap::visit_children_of;
use crate::{
    CellKind, CellType, CellTypeRange, Class, ClassOps, Extends, Field, Gc, GcFree, GlobalObject,
    JsCell, JsObject, JsObjectInit, PropertyOffset, Shape, ShapeSpec, TypeFlags, Value, Visitor,
    Vm, ascii_literal,
};

/// An ordinary object. The first slots follow the header.
#[repr(C)]
pub struct JsFinalObject {
    base: JsObject,
}

#[unsafe(export_name = "engine_class_FinalObject")]
static FINAL_OBJECT_CLASS: Class = Class::new::<JsFinalObject>(
    ascii_literal!("Object"),
    None,
    CellTypeRange::new(CellType::FinalObject, CellType::FinalObject),
);

impl !GcFree for JsFinalObject {}

impl Deref for JsFinalObject {
    type Target = JsObject;

    #[inline(always)]
    fn deref(&self) -> &JsObject {
        &self.base
    }
}

// SAFETY: starts with a `JsObject`.
unsafe impl<T> Extends<T> for JsFinalObject where JsObject: Extends<T> {}

/// Only `JsFinalObject::allocate` knows the size.
pub enum Never {}

// SAFETY: visits the slots too.
unsafe impl CellKind for JsFinalObject {
    type Base = JsObject;
    type Init = Never;

    const CLASS: &'static Class = &FINAL_OBJECT_CLASS;
    const CELL_TYPE: CellType = CellType::FinalObject;
    const FLAGS: TypeFlags = JsObject::FLAGS;
    const HOLDS_CLASS: bool = false;
    const OPS: ClassOps = {
        let mut ops = JsObject::OPS;
        ops.visit_children = visit_children_of::<JsFinalObject>;
        ops
    };

    #[inline(always)]
    fn is(cell: &JsCell) -> bool {
        cell.cell_type() == CellType::FinalObject
    }

    fn shape_of(init: &Never) -> Gc<Shape> {
        match *init {}
    }

    unsafe fn initialize(_: *mut JsFinalObject, init: Never) {
        match init {}
    }

    #[inline(always)]
    fn trace_fields(&self, visitor: &mut Visitor) {
        let Some(max_offset) = self.visit_shape_and_storage(visitor) else {
            return;
        };
        let count = if max_offset.is_inline() {
            (max_offset.get() + 1) as usize
        } else {
            let shape = self.shape_id_concurrently().address();
            // SAFETY: all shapes that the object ever has are of the same capacity.
            unsafe { Gc::<Shape>::from_address(shape) }.inline_capacity()
        };
        // SAFETY: at most the capacity.
        super::visit::append_hidden(unsafe { self.inline_slots(count) }, visitor);
    }
}

impl JsFinalObject {
    pub const DEFAULT_INLINE_CAPACITY: usize = 6;
    pub const MAX_INLINE_CAPACITY: usize = 62;

    #[inline(always)]
    pub const fn allocation_size(inline_capacity: usize) -> usize {
        size_of::<JsObject>() + inline_capacity * size_of::<u64>()
    }

    /// For an object that is known to get `inferred` properties: the cell is filled up with slots.
    pub fn optimal_inline_capacity(inferred: usize) -> usize {
        const SLACK_BELOW: usize = 2;
        let wanted = match inferred {
            0 => Self::DEFAULT_INLINE_CAPACITY,
            _ => inferred.clamp(SLACK_BELOW, Self::MAX_INLINE_CAPACITY),
        };
        let size = crate::heap::optimal_size(Self::allocation_size(wanted));
        ((size - size_of::<JsObject>()) / size_of::<u64>()).min(Self::MAX_INLINE_CAPACITY)
    }

    pub fn create_shape(
        vm: &Vm,
        global: Gc<GlobalObject>,
        prototype: Value,
        inline_capacity: usize,
    ) -> Gc<Shape> {
        assert!(inline_capacity <= Self::MAX_INLINE_CAPACITY);
        let spec = ShapeSpec::of::<JsFinalObject>().with_inline_capacity(inline_capacity);
        Shape::create(vm, Some(global), prototype, spec)
    }

    #[inline(always)]
    unsafe fn inline_slots(&self, count: usize) -> &[Field<Option<Value>>] {
        let first = core::ptr::with_exposed_provenance(self.address() + size_of::<JsObject>());
        // SAFETY: the caller's.
        unsafe { core::slice::from_raw_parts(first, count) }
    }

    /// The slots in the cell are empty.
    #[inline(always)]
    pub(crate) fn allocate(vm: &Vm, init: JsObjectInit) -> Gc<JsFinalObject> {
        let shape = JsObject::shape_of(&init);
        assert!(shape.instance_type() == CellType::FinalObject);
        let capacity = shape.inline_capacity();
        let address = vm.allocate_cell_memory(Self::allocation_size(capacity));
        let object = core::ptr::with_exposed_provenance_mut::<JsObject>(address);
        // SAFETY: a new cell of that size.
        unsafe {
            JsObject::initialize(object, init);
            object.add(1).cast::<u64>().write_bytes(0, capacity);
            crate::heap::barrier::publish_fence();
            Gc::from_address(address)
        }
    }

    /// The properties that the shape has are undefined.
    #[inline]
    pub fn create(vm: &Vm, shape: Gc<Shape>) -> Gc<JsFinalObject> {
        let object = Self::allocate(vm, JsObjectInit::new(vm, shape));
        if shape.max_offset().is_valid() {
            // SAFETY: at most the capacity, and no cell.
            for slot in unsafe { object.inline_slots(shape.inline_size()) } {
                // SAFETY: as above.
                unsafe { slot.set_without_barrier(Some(Value::UNDEFINED)) };
            }
        }
        object
    }

    /// One value for each slot of the shape, in order. Panics if there are more or fewer, or any is an accessor.
    #[inline]
    pub fn create_with_values(vm: &Vm, shape: Gc<Shape>, values: &[Value]) -> Gc<JsFinalObject> {
        assert!(
            values.len() == shape.slot_count() && !shape.has(crate::ShapeFlags::HAS_ANY_ACCESSOR)
        );
        let object = Self::allocate(vm, JsObjectInit::new(vm, shape));
        let capacity = shape.inline_capacity();
        for (number, &value) in values.iter().enumerate() {
            let offset = PropertyOffset::for_slot(number, capacity);
            // SAFETY: a slot of the shape, in a new object.
            unsafe {
                object
                    .slot_unchecked(offset)
                    .set_without_barrier(Some(value))
            };
        }
        object
    }
}
