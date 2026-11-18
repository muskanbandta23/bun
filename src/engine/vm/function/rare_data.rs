use core::cell::Cell;
use core::mem::offset_of;

use bun_engine_macros::cell;

use crate::heap::Allocator;
use crate::{
    Executable, Field, Gc, InlineWatchpointSet, JsFinalObject, JsFunction, Shape, Vm,
    WatchpointState, call,
};

/// What a constructor knows of the objects that it makes. It stands in for the executable, and forwards calls to it.
#[cell(cell_type = FunctionRareData)]
pub struct FunctionRareData {
    base: Executable,
    executable: Gc<Executable>,
    /// Of `this`.
    pub object_shape: Field<Option<Gc<Shape>>>,
    object_allocator: Cell<*const Allocator>,
    /// Of what a native constructor makes when this function is `new.target`.
    pub(crate) subclass_shape: Field<Option<Gc<Shape>>>,
    pub(crate) bound_function_shape: Field<Option<Gc<Shape>>>,
    /// Fires when `prototype` changes.
    allocation_watchpoints: InlineWatchpointSet,
}

impl FunctionRareData {
    pub const OFFSET_OF_EXECUTABLE: usize = offset_of!(FunctionRareData, executable);
    pub const OFFSET_OF_OBJECT_SHAPE: usize = offset_of!(FunctionRareData, object_shape);
    pub const OFFSET_OF_OBJECT_ALLOCATOR: usize = offset_of!(FunctionRareData, object_allocator);

    pub(super) fn create(vm: &Vm, executable: Gc<Executable>) -> Gc<FunctionRareData> {
        let (call, construct) = (
            call::rare_data_call_entry(),
            call::rare_data_construct_entry(),
        );
        vm.allocate::<FunctionRareData>(FunctionRareDataInit {
            base: Executable::init(vm.shapes.function_rare_data(), call, construct),
            executable,
            object_shape: None,
            object_allocator: Cell::new(core::ptr::null()),
            subclass_shape: None,
            bound_function_shape: None,
            allocation_watchpoints: InlineWatchpointSet::new(WatchpointState::Clear),
        })
    }

    #[inline(always)]
    pub fn executable(&self) -> Gc<Executable> {
        self.executable
    }

    #[inline(always)]
    pub fn object_allocator(&self) -> Option<&Allocator> {
        // SAFETY: null, or a part of the heap.
        unsafe { self.object_allocator.get().as_ref() }
    }

    pub fn allocation_watchpoints(&self) -> &InlineWatchpointSet {
        &self.allocation_watchpoints
    }

    pub(crate) fn clear(&self, vm: &Vm) {
        self.set_object_shape(None);
        self.object_allocator.set(core::ptr::null());
        self.set_subclass_shape(None);
        self.allocation_watchpoints.fire(vm);
    }
}

impl JsFunction {
    /// The shape of `this` when this function is `new.target`. `can_use_allocation_profile` holds.
    #[inline]
    pub fn shape_for_construction(&self, vm: &Vm, inline_capacity: usize) -> Gc<Shape> {
        match self.rare_data().and_then(|it| it.object_shape()) {
            Some(shape) => shape,
            None => self.initialize_allocation_profile(vm, inline_capacity),
        }
    }

    #[inline(never)]
    fn initialize_allocation_profile(&self, vm: &Vm, inferred_capacity: usize) -> Gc<Shape> {
        assert!(self.can_use_allocation_profile());
        let prototype = self.prototype_for_construction(vm);
        let rare_data = self.ensure_rare_data(vm);
        let capacity = JsFinalObject::optimal_inline_capacity(inferred_capacity);
        let shape = Shape::for_empty_object(vm, self.function_realm(), prototype, capacity);
        let size = JsFinalObject::allocation_size(capacity);
        let allocator = vm.heap.cells.allocator_for(size);
        rare_data.set_object_shape(Some(shape));
        rare_data
            .object_allocator
            .set(allocator.map_or(core::ptr::null(), core::ptr::from_ref));
        rare_data.allocation_watchpoints.start_watching();
        shape
    }
}
