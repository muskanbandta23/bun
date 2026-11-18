use core::any::{Any, TypeId};
use core::cell::{Cell, RefCell, UnsafeCell};
use core::ffi::c_void;
use core::mem::{MaybeUninit, needs_drop, offset_of};
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};
use std::rc::Rc;

use bun_engine_bytecode::Specialization;
use bun_engine_macros::FieldOffsets;

use crate::code::NativeExecutableKey;
use crate::function::CustomAccessorKey;
use crate::heap::stack::{self, StackBounds, stack_pointer};
use crate::heap::{Heap, HeapKind, IsoSpace, Scope, Survivors, WeakValueMap, barrier};
use crate::object::ShapeCache;
use crate::primitive::{StringCaches, WeaklyHeldSymbols};
use crate::services::Service;
use crate::{
    CallFrame, CellKind, CellType, Cx, ElementKind, EntryRecord, Exception, Executable,
    FastHashMap, Gc, GcFree, GlobalObject, Hooks, IndexingMode, JsBoundFunction, JsCell,
    JsFunction, JsSymbol, MicrotaskQueueRef, NativeExecutable, NumberStrings, Shape, ShapeSpec,
    SmallStrings, Traps, TypeFlags, Value, Visitor, call, options,
};

/// A cell that the `Vm` keeps alive.
#[repr(transparent)]
pub(crate) struct Root<T>(Cell<Option<Gc<T>>>);

impl<T: CellKind> Root<T> {
    pub(crate) const fn new() -> Root<T> {
        Root(Cell::new(None))
    }

    #[inline(always)]
    pub(crate) fn get(&self) -> Option<Gc<T>> {
        self.0.get()
    }

    #[inline(always)]
    pub(crate) fn set(&self, cell: Option<Gc<T>>) {
        self.0.set(cell);
    }

    pub(crate) fn visit(&self, visitor: &mut Visitor) {
        crate::Trace::trace(&self.get(), visitor);
    }
}

macro_rules! vm_shapes {
    ($($name:ident: $type:ty,)*) => {
        /// The shapes of the cells that belong to no realm.
        #[repr(C)]
        pub struct VmShapes {
            shape: Root<Shape>,
            immutable_arrays: [Root<Shape>; 3],
            $($name: Root<Shape>,)*
        }

        impl VmShapes {
            const fn new() -> VmShapes {
                VmShapes {
                    shape: Root::new(),
                    immutable_arrays: [const { Root::new() }; 3],
                    $($name: Root::new(),)*
                }
            }

            fn initialize(&self, vm: &Vm) {
                self.shape.set(Some(Shape::bootstrap(vm)));
                $(self.$name.set(Some(Shape::create(vm, None, Value::NULL, ShapeSpec::of::<$type>())));)*
            }

            fn visit(&self, visitor: &mut Visitor) {
                self.shape.visit(visitor);
                self.immutable_arrays.iter().for_each(|shape| shape.visit(visitor));
                $(self.$name.visit(visitor);)*
            }

            $(
                #[inline(always)]
                pub fn $name(&self) -> Gc<Shape> {
                    // SAFETY: set when the `Vm` is made.
                    unsafe { self.$name.get().unwrap_unchecked() }
                }
            )*
        }
    };
}

vm_shapes! {
    string: crate::JsString,
    symbol: crate::JsSymbol,
    bigint: crate::JsBigInt,
    getter_setter: crate::GetterSetter,
    custom_getter_setter: crate::CustomGetterSetter,
    sparse_elements: crate::SparseElements,
    executable: crate::Executable,
    native_executable: crate::NativeExecutable,
    program_executable: crate::ProgramExecutable,
    module_executable: crate::ModuleExecutable,
    eval_executable: crate::EvalExecutable,
    function_executable: crate::FunctionExecutable,
    function_rare_data: crate::FunctionRareData,
    code_block: crate::CodeBlock,
    scope_template: crate::ScopeTemplate,
    exception: crate::Exception,
    regexp: crate::RegExp,
    property_name_enumerator: crate::PropertyNameEnumerator,
    sentinel: crate::Sentinel,
    slim_promise_reaction: crate::PromiseReaction,
    full_promise_reaction: crate::FullPromiseReaction,
    promise_combinator: crate::PromiseCombinator,
}

impl VmShapes {
    #[inline(always)]
    pub fn shape(&self) -> Gc<Shape> {
        // SAFETY: as above.
        unsafe { self.shape.get().unwrap_unchecked() }
    }

    #[inline(always)]
    pub fn immutable_array(&self, kind: ElementKind) -> Gc<Shape> {
        let index = (kind as usize - ElementKind::Int32 as usize) / 2;
        self.immutable_arrays[index].get().expect("a dense kind")
    }
}

/// What the embedder has registered.
#[derive(Default)]
pub struct Embedder {
    pub debugger_break: Cell<Option<fn(&mut Cx)>>,
    pub shell_timeout_check: Cell<Option<fn(&mut Cx)>>,
    pub reconcile_weak_references: Cell<Option<fn(&Vm, &Survivors)>>,
    /// After the last cell.
    pub destroy_members: Cell<Option<fn(&Vm)>>,
    /// AgentCanSuspend is false.
    pub must_not_block: Cell<bool>,
    pub errors: crate::ErrorHooks,
    pub console: Cell<Option<&'static crate::ConsoleHooks>>,
    pub microtasks: crate::MicrotaskHooks,
}

/// All that the scripts of one thread share.
#[derive(FieldOffsets)]
#[repr(C)]
pub struct Vm {
    exception: Root<Exception>,
    traps: Traps,
    top_call_frame: Cell<usize>,
    top_entry_frame: Cell<usize>,
    shape_base: usize,
    catch_frame: Cell<usize>,
    catch_address: Cell<usize>,
    catch_bytecode_offset: Cell<u32>,
    /// Changes when a property of a prototype does, and with each collection. The low 31 bits are never zero.
    prototype_chain_epoch: Cell<u64>,
    pub client_data: Cell<*mut c_void>,
    entry_global: Root<GlobalObject>,
    termination_exception: Root<Exception>,
    last_exception: Root<Exception>,
    /// For generated code to pass values through.
    scratch: [Cell<u64>; 4],
    internal_function_executable: Root<Executable>,
    ref_count: AtomicU32,
    pub did_enter: Cell<bool>,
    is_execution_forbidden: Cell<bool>,
    deletes_unconfigurable_properties: Cell<bool>,
    pub(crate) is_async_context_tracked: Cell<bool>,
    /// Members of the class that C++ sees.
    embedder_members: UnsafeCell<MaybeUninit<[usize; 48]>>,
    pub heap: Heap,
    heap_padding: MaybeUninit<[u8; Heap::SIZE_LIMIT - size_of::<Heap>()]>,
    /// The last field whose offset C++ knows.
    pub small_strings: SmallStrings,

    pub shapes: VmShapes,
    hooks: &'static Hooks,
    embedder: Embedder,
    stack_bounds: StackBounds,
    lowest_stack_pointer: Cell<usize>,
    is_handling_error: Cell<bool>,
    debugger_count: Cell<u32>,
    seed: Cell<u64>,
    when_idle: RefCell<Vec<Box<dyn FnOnce(&Vm) + GcFree>>>,
    conservative_ranges: RefCell<Vec<Range<usize>>>,
    bound_function_executable: Root<NativeExecutable>,
    spaces_by_class: RefCell<FastHashMap<usize, &'static IsoSpace>>,
    shapes_by_class: RefCell<FastHashMap<usize, usize>>,
    extensions: RefCell<FastHashMap<TypeId, Rc<dyn Any>>>,
    default_microtask_queue: MicrotaskQueueRef,
    pub(crate) string_caches: StringCaches,
    pub(crate) number_strings: NumberStrings,
    pub(crate) shape_cache: ShapeCache,
    pub(crate) symbols: WeakValueMap<usize, JsSymbol>,
    pub(crate) weakly_held_symbols: WeaklyHeldSymbols,
    pub(crate) native_executables: WeakValueMap<NativeExecutableKey, NativeExecutable>,
    pub(crate) custom_accessor_functions: WeakValueMap<CustomAccessorKey, JsFunction>,
    pub(crate) services: crate::services::Services,
}

// SAFETY: visits its roots.
unsafe impl GcFree for Vm {}

/// A table that is too large to be made on the stack.
///
/// # Safety
/// `complete` makes a valid value of zeros.
pub(crate) unsafe trait StartsZeroed {
    /// # Safety
    /// Memory for a `Self` that is all zero.
    unsafe fn complete(_: *mut Self) {}
}

/// `Box::new(Vm { .. })`, field by field.
macro_rules! new_vm {
    (zeroed: [$($table:ident),*], $($field:ident: $value:expr,)*) => {{
        let _has_no_other_fields = |Vm { $($table: _,)* $($field: _,)* }: &Vm| ();
        let mut vm = Box::<Vm>::new_zeroed();
        let fields = vm.as_mut_ptr();
        $(
            let value = $value;
            // SAFETY: inside the allocation.
            unsafe { (&raw mut (*fields).$field).write(value) };
        )*
        // SAFETY: all fields are initialized.
        unsafe {
            $(StartsZeroed::complete(&raw mut (*fields).$table);)*
            vm.assume_init()
        }
    }};
}

// SAFETY: anything goes.
unsafe impl<T> StartsZeroed for MaybeUninit<T> {}

/// A counted reference. The last one has to go on the thread of the `Vm`.
pub struct VmRef(NonNull<Vm>);

impl VmRef {
    /// `set_options` has been called.
    pub fn new(hooks: &'static Hooks, heap_kind: HeapKind) -> VmRef {
        let mut seed = [0; 8];
        getrandom::fill(&mut seed).expect("a source of randomness");
        let seed = u64::from_ne_bytes(seed);
        let heap = Heap::new(heap_kind, seed);
        let vm: &Vm = Box::leak(new_vm! {
            zeroed: [heap_padding, small_strings, string_caches, number_strings],
            exception: Root::new(),
            top_call_frame: Cell::new(0),
            top_entry_frame: Cell::new(0),
            traps: Traps::new(),
            shape_base: crate::heap::shape_base(),
            catch_frame: Cell::new(0),
            catch_address: Cell::new(0),
            catch_bytecode_offset: Cell::new(0),
            prototype_chain_epoch: Cell::new(1),
            client_data: Cell::new(core::ptr::null_mut()),
            entry_global: Root::new(),
            termination_exception: Root::new(),
            last_exception: Root::new(),
            scratch: Default::default(),
            shapes: VmShapes::new(),
            embedder_members: UnsafeCell::new(MaybeUninit::zeroed()),
            heap: heap,
            ref_count: AtomicU32::new(1),
            hooks: hooks,
            embedder: Embedder::default(),
            stack_bounds: StackBounds::current(),
            lowest_stack_pointer: Cell::new(stack_pointer()),
            is_handling_error: Cell::new(false),
            is_execution_forbidden: Cell::new(false),
            deletes_unconfigurable_properties: Cell::new(false),
            is_async_context_tracked: Cell::new(false),
            debugger_count: Cell::new(0),
            did_enter: Cell::new(false),
            seed: Cell::new(seed.rotate_left(32) | 1),
            when_idle: RefCell::default(),
            conservative_ranges: RefCell::default(),
            internal_function_executable: Root::new(),
            bound_function_executable: Root::new(),
            spaces_by_class: RefCell::default(),
            shapes_by_class: RefCell::default(),
            extensions: RefCell::default(),
            default_microtask_queue: MicrotaskQueueRef::unregistered(),
            shape_cache: ShapeCache::default(),
            symbols: WeakValueMap::default(),
            weakly_held_symbols: WeaklyHeldSymbols::default(),
            native_executables: WeakValueMap::default(),
            custom_accessor_functions: WeakValueMap::default(),
            services: Default::default(),
        });
        core::ptr::from_ref(vm).expose_provenance();
        vm.initialize();
        VmRef(NonNull::from(vm))
    }

    /// # Safety
    /// As for `Cx::from_global`.
    pub unsafe fn get(&self) -> &Vm {
        // SAFETY: counted.
        unsafe { self.0.as_ref() }
    }

    pub fn into_raw(self) -> NonNull<Vm> {
        core::mem::ManuallyDrop::new(self).0
    }

    /// # Safety
    /// The caller hands over a reference that it holds.
    pub unsafe fn from_raw(vm: NonNull<Vm>) -> VmRef {
        VmRef(vm)
    }

    pub fn retain(vm: &Vm) -> VmRef {
        vm.ref_count.fetch_add(1, Ordering::Relaxed);
        VmRef(NonNull::from(vm))
    }
}

impl Drop for VmRef {
    fn drop(&mut self) {
        // SAFETY: counted. Without a reference nothing can get at a cell any more.
        unsafe {
            let vm = self.0.as_ref();
            if vm.ref_count.fetch_sub(1, Ordering::AcqRel) == 1 {
                vm.services.will_destroy(vm);
                vm.heap.tear_down();
                if let Some(destroy) = vm.embedder.destroy_members.get() {
                    destroy(vm);
                }
                drop(Box::from_raw(self.0.as_ptr()));
            }
        }
    }
}

pub struct SuspendException {
    vm: NonNull<Vm>,
    exception: Option<Gc<Exception>>,
    last_exception: Option<Gc<Exception>>,
}

impl Drop for SuspendException {
    fn drop(&mut self) {
        // SAFETY: made and dropped inside a call that has the `Vm`.
        let vm = unsafe { self.vm.as_ref() };
        vm.exception.set(self.exception);
        vm.last_exception.set(self.last_exception);
    }
}

impl Vm {
    pub const OFFSET_OF_EXCEPTION: usize = offset_of!(Vm, exception);
    pub const OFFSET_OF_TOP_CALL_FRAME: usize = offset_of!(Vm, top_call_frame);
    pub const OFFSET_OF_TOP_ENTRY_FRAME: usize = offset_of!(Vm, top_entry_frame);
    pub const OFFSET_OF_STACK_LIMIT: usize = offset_of!(Vm, traps) + Traps::OFFSET_OF_STACK_LIMIT;
    pub const OFFSET_OF_REAL_STACK_LIMIT: usize =
        offset_of!(Vm, traps) + Traps::OFFSET_OF_REAL_STACK_LIMIT;
    pub const OFFSET_OF_PENDING_TRAPS: usize = offset_of!(Vm, traps) + Traps::OFFSET_OF_PENDING;
    pub const OFFSET_OF_SHAPE_BASE: usize = offset_of!(Vm, shape_base);
    pub const OFFSET_OF_CATCH_FRAME: usize = offset_of!(Vm, catch_frame);
    pub const OFFSET_OF_CATCH_ADDRESS: usize = offset_of!(Vm, catch_address);
    pub const OFFSET_OF_CATCH_BYTECODE_OFFSET: usize = offset_of!(Vm, catch_bytecode_offset);
    pub const OFFSET_OF_PROTOTYPE_CHAIN_EPOCH: usize = offset_of!(Vm, prototype_chain_epoch);
    /// Of a pointer to the `LookupTables`.
    pub const OFFSET_OF_LOOKUP_TABLES: usize =
        offset_of!(Vm, services.caches) + crate::caches::State::OFFSET_OF_TABLES;
    pub const OFFSET_OF_SCRATCH: usize = offset_of!(Vm, scratch);
    pub const OFFSET_OF_IS_ASYNC_CONTEXT_TRACKED: usize = offset_of!(Vm, is_async_context_tracked);
    pub const OFFSET_OF_DEBUGGER_COUNT: usize = offset_of!(Vm, debugger_count);
    pub const OFFSET_OF_HEAP: usize = offset_of!(Vm, heap);
    pub const OFFSET_OF_SMALL_STRINGS: usize = offset_of!(Vm, small_strings);
    pub const OFFSET_OF_NUMBER_STRINGS: usize = offset_of!(Vm, number_strings);

    fn initialize(&self) {
        self.update_stack_limit();
        self.shapes.initialize(self);
        let kinds = [
            ElementKind::Int32,
            ElementKind::Double,
            ElementKind::Contiguous,
        ];
        for (root, kind) in self.shapes.immutable_arrays.iter().zip(kinds) {
            let mode = IndexingMode::array(kind).copy_on_write();
            let spec = ShapeSpec::of::<crate::ImmutableArray>().with_indexing_mode(mode);
            root.set(Some(Shape::create(self, None, Value::NULL, spec)));
        }
        self.small_strings.initialize(self);
        self.number_strings.initialize(self);
        let init = Executable::init(
            self.shapes.executable(),
            call::internal_function_call_entry(),
            call::internal_function_construct_entry(),
        );
        self.internal_function_executable
            .set(Some(self.allocate::<Executable>(init)));
        self.termination_exception
            .set(Some(Exception::create_termination(self)));
        self.services.initialize(self);
        self.heap.notify_is_safe_to_collect();
    }

    #[inline(always)]
    pub fn hooks(&self) -> &'static Hooks {
        self.hooks
    }

    #[inline(always)]
    pub fn embedder(&self) -> &Embedder {
        &self.embedder
    }

    #[inline(always)]
    pub fn traps(&self) -> &Traps {
        &self.traps
    }

    /// Ends the process if there is no memory.
    #[inline(always)]
    pub(crate) fn allocate_cell_memory(&self, size: usize) -> usize {
        let address = self.heap.cells.try_allocate(&self.heap, size);
        if address == 0 {
            bun_alloc::out_of_memory();
        }
        address
    }

    /// Panics unless the shape is one of instances of `T`.
    #[inline(always)]
    pub fn allocate<T: CellKind>(&self, init: T::Init) -> Gc<T> {
        assert!(T::shape_of(&init).class().is(T::CLASS));
        let (heap, size) = (&self.heap, size_of::<T>());
        let address = if !needs_drop::<T>() {
            heap.cells.try_allocate(heap, size)
        } else if T::CELL_TYPE == CellType::String {
            heap.strings.allocator.try_allocate_sized(heap, size)
        } else if T::CELL_TYPE == CellType::Shape {
            heap.shapes.allocator.try_allocate(heap)
        } else if T::HOLDS_CLASS {
            heap.destructible_objects.try_allocate(heap, size)
        } else if T::FLAGS.contains(TypeFlags::SHAPE_IS_IMMORTAL) {
            heap.destructible_cells.try_allocate(heap, size)
        } else {
            self.space_for::<T>().allocator.try_allocate(heap)
        };
        if address == 0 {
            bun_alloc::out_of_memory();
        }
        // SAFETY: a new cell that is large enough, in a space that destroys it as a `T`.
        unsafe {
            T::initialize(core::ptr::with_exposed_provenance_mut(address), init);
            barrier::publish_fence();
            Gc::from_address(address)
        }
    }

    #[inline(never)]
    fn space_for<T: CellKind>(&self) -> &IsoSpace {
        let key = core::ptr::from_ref(T::CLASS).addr();
        self.spaces_by_class
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let space = self
                    .heap
                    .create_iso_space(size_of::<T>(), Some(crate::heap::destroy::<T>));
                // SAFETY: lives as long as the heap.
                unsafe { &*core::ptr::from_ref(space) }
            })
    }

    /// The shape of a type of cell that is no object and is not among `shapes`.
    pub fn shape_for<T: CellKind>(&self) -> Gc<Shape> {
        const { assert!(!T::CELL_TYPE.is_object()) };
        let key = core::ptr::from_ref(T::CLASS).addr();
        if let Some(&address) = self.shapes_by_class.borrow().get(&key) {
            // SAFETY: a root.
            return unsafe { Gc::from_address(address) };
        }
        let shape = Shape::create(self, None, Value::NULL, ShapeSpec::of::<T>());
        self.shapes_by_class
            .borrow_mut()
            .insert(key, shape.address());
        shape
    }

    /// State that another crate keeps for the `Vm`.
    pub fn extension<T: Default + GcFree + 'static>(&self) -> Rc<T> {
        let existing = self.extensions.borrow().get(&TypeId::of::<T>()).cloned();
        if let Some(Ok(existing)) = existing.map(Rc::downcast) {
            return existing;
        }
        let new = Rc::new(T::default());
        let mut extensions = self.extensions.borrow_mut();
        extensions.insert(TypeId::of::<T>(), Rc::<T>::clone(&new));
        new
    }

    pub fn random_seed(&self) -> u64 {
        let state = self.seed.get().wrapping_add(0x9e37_79b9_7f4a_7c15);
        self.seed.set(state);
        let mixed = (state ^ state >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let mixed = (mixed ^ mixed >> 27).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ mixed >> 31
    }

    pub fn internal_function_executable(&self) -> Gc<Executable> {
        self.internal_function_executable
            .get()
            .expect("set when the Vm is made")
    }

    pub fn bound_function_executable(&self) -> Gc<NativeExecutable> {
        self.bound_function_executable.get().unwrap_or_else(|| {
            let executable = JsBoundFunction::create_executable(self);
            self.bound_function_executable.set(Some(executable));
            executable
        })
    }

    pub fn default_microtask_queue(&self) -> &MicrotaskQueueRef {
        &self.default_microtask_queue
    }

    #[inline(always)]
    pub fn has_exception(&self) -> bool {
        self.exception.get().is_some()
    }

    #[inline(always)]
    pub fn exception(&self) -> Option<Gc<Exception>> {
        self.exception.get()
    }

    pub fn last_exception(&self) -> Option<Gc<Exception>> {
        self.last_exception.get()
    }

    pub fn clear_last_exception(&self) {
        self.last_exception.set(None);
    }

    #[inline]
    pub(crate) fn set_exception(&self, exception: Gc<Exception>) {
        self.exception.set(Some(exception));
        self.last_exception.set(Some(exception));
    }

    /// Whatever it is.
    #[inline]
    pub fn clear_exception(&self) -> Option<Gc<Exception>> {
        let exception = self.exception.get();
        self.exception.set(None);
        exception
    }

    /// While it is alive no exception is pending, and then the one that was is again.
    pub fn suspend_exception(&self) -> SuspendException {
        let (exception, last_exception) = (self.clear_exception(), self.last_exception());
        self.clear_last_exception();
        SuspendException {
            vm: NonNull::from(self),
            exception,
            last_exception,
        }
    }

    #[inline]
    pub fn termination_exception(&self) -> Gc<Exception> {
        self.termination_exception
            .get()
            .expect("set when the Vm is made")
    }

    #[inline]
    pub fn is_termination_exception(&self, exception: Gc<Exception>) -> bool {
        self.termination_exception.get() == Some(exception)
    }

    #[inline]
    pub fn has_pending_termination_exception(&self) -> bool {
        self.has_exception() && self.exception.get() == self.termination_exception.get()
    }

    pub fn is_execution_forbidden(&self) -> &Cell<bool> {
        &self.is_execution_forbidden
    }

    #[inline(always)]
    pub fn deletes_unconfigurable_properties(&self) -> bool {
        self.deletes_unconfigurable_properties.get()
    }

    pub fn replace_deletes_unconfigurable_properties(&self, value: bool) -> bool {
        self.deletes_unconfigurable_properties.replace(value)
    }

    #[inline(always)]
    pub fn has_debugger(&self) -> bool {
        self.debugger_count.get() != 0
    }

    pub fn debugger_count(&self) -> &Cell<u32> {
        &self.debugger_count
    }

    #[inline(always)]
    pub fn set_top_call_frame(&self, frame: &CallFrame) {
        self.top_call_frame.set(frame.address());
    }

    /// The innermost frame that has called out of generated code.
    #[inline(always)]
    pub fn top_call_frame(&self) -> Option<&CallFrame> {
        let address = self.top_call_frame.get();
        // SAFETY: it is there for as long as the native code that it has called runs.
        (address != 0).then(|| unsafe { CallFrame::from_address(address) })
    }

    #[inline(always)]
    pub fn top_entry_frame(&self) -> usize {
        self.top_entry_frame.get()
    }

    /// The frames of the stack, innermost first. Calls that were inlined have none.
    pub fn machine_frames(&self) -> impl Iterator<Item = &CallFrame> {
        let mut entry_frame = self.top_entry_frame();
        core::iter::successors(self.top_call_frame(), move |frame| {
            let mut caller = frame.caller_address();
            if caller == entry_frame {
                // SAFETY: further up the stack.
                let record = unsafe { EntryRecord::of(entry_frame) };
                caller = record.previous_top_call_frame;
                entry_frame = record.previous_top_entry_frame;
            }
            // SAFETY: as above.
            (caller != 0).then(|| unsafe { CallFrame::from_address(caller) })
        })
    }

    pub(crate) fn set_catch_target(&self, frame: usize, address: usize, bytecode_offset: u32) {
        self.catch_frame.set(frame);
        self.catch_address.set(address);
        self.catch_bytecode_offset.set(bytecode_offset);
    }

    pub fn catch_bytecode_offset(&self) -> u32 {
        self.catch_bytecode_offset.get()
    }

    #[inline(always)]
    pub fn is_entered(&self) -> bool {
        self.entry_global.get().is_some()
    }

    pub fn entry_global(&self) -> Option<Gc<GlobalObject>> {
        self.entry_global.get()
    }

    #[inline(always)]
    pub(crate) fn set_entry_global(&self, global: Gc<GlobalObject>) {
        self.entry_global.set(Some(global));
    }

    pub(crate) fn will_enter(&self, global: Gc<GlobalObject>) {
        self.entry_global.set(Some(global));
        self.did_enter.set(true);
        self.clear_last_exception();
        self.clear_dead_stack();
        self.services.will_enter(self);
    }

    pub(crate) fn did_leave(&self) {
        self.entry_global.set(None);
        self.reset_termination_request();
        if !self.when_idle.borrow().is_empty() {
            for callback in self.when_idle.take() {
                callback(self);
            }
        }
    }

    pub fn when_idle(&self, callback: impl FnOnce(&Vm) + GcFree + 'static) {
        if self.is_entered() {
            self.when_idle.borrow_mut().push(Box::new(callback));
        } else {
            callback(self);
        }
    }

    pub fn stack_bounds(&self) -> StackBounds {
        self.stack_bounds
    }

    #[inline(always)]
    pub fn soft_stack_limit(&self) -> usize {
        self.traps.real_stack_limit()
    }

    fn stack_limit_with_reserve(&self, reserved: usize) -> usize {
        let StackBounds { origin, end } = self.stack_bounds;
        let usable = (origin - end).min(options().max_per_thread_stack_usage);
        origin - usable.saturating_sub(reserved)
    }

    /// For native code that does not call out, such as a parser.
    pub fn hard_stack_limit(&self) -> usize {
        self.stack_limit_with_reserve(options().reserved_zone_size)
    }

    fn update_stack_limit(&self) {
        let reserved = if self.is_handling_error.get() {
            options().reserved_zone_size
        } else {
            options().soft_reserved_zone_size
        };
        self.traps
            .set_real_stack_limit(self.stack_limit_with_reserve(reserved));
    }

    /// Making an error may use some of the stack that is otherwise held back. Returns the old setting.
    pub fn replace_is_handling_error(&self, value: bool) -> bool {
        let old = self.is_handling_error.replace(value);
        self.update_stack_limit();
        old
    }

    #[inline]
    pub fn clear_dead_stack(&self) {
        let current = stack_pointer();
        let lowest = self.lowest_stack_pointer.replace(current);
        if lowest < current {
            // SAFETY: a value that the stack pointer of this thread has had.
            unsafe { stack::clear_below_stack_pointer(lowest) };
        }
    }

    #[inline(always)]
    pub fn prototype_chain_epoch(&self) -> u32 {
        self.prototype_chain_epoch.get() as u32
    }

    /// For what is not cleared when the other starts over.
    #[inline(always)]
    pub fn full_prototype_chain_epoch(&self) -> u64 {
        self.prototype_chain_epoch.get()
    }

    #[inline]
    pub fn invalidate_prototype_chain_caches(&self) {
        self.prototype_chain_epoch.update(|epoch| epoch + 1);
        if self.prototype_chain_epoch() << 1 == 0 {
            self.services.caches.did_wrap_epoch();
            self.prototype_chain_epoch.update(|epoch| epoch + 1);
        }
    }

    /// # Safety
    /// The memory stays mapped until it is removed.
    pub unsafe fn add_conservative_range(&self, range: Range<usize>) {
        self.conservative_ranges.borrow_mut().push(range);
    }

    pub fn remove_conservative_range(&self, range: &Range<usize>) {
        self.conservative_ranges
            .borrow_mut()
            .retain(|it| it != range);
    }

    pub(crate) fn for_each_conservative_range(&self, mut visit: impl FnMut(usize, usize)) {
        for range in self.conservative_ranges.borrow().iter() {
            visit(range.start, range.end);
        }
        let scratch = self.scratch.as_ptr_range();
        visit(scratch.start.addr(), scratch.end.addr());
    }

    pub(crate) fn code_address_range(&self) -> Range<usize> {
        bun_engine_exec_memory::address_range()
    }

    pub(crate) fn will_collect(&self, _: Scope) {
        // They go on writing their profiles without a barrier.
        for code_block in self.machine_frames().filter_map(CallFrame::code_block) {
            code_block.write_barrier_for_cell();
        }
    }

    pub(crate) fn visit_roots(&self, visitor: &mut Visitor) {
        self.exception.visit(visitor);
        self.last_exception.visit(visitor);
        self.termination_exception.visit(visitor);
        self.entry_global.visit(visitor);
        self.internal_function_executable.visit(visitor);
        self.bound_function_executable.visit(visitor);
        self.shapes.visit(visitor);
        self.small_strings.visit(visitor);
        self.number_strings.visit(visitor);
        self.weakly_held_symbols.visit(visitor);
        for &shape in self.shapes_by_class.borrow().values() {
            // SAFETY: a root.
            unsafe { visitor.append_word(shape as u64) };
        }
        self.services.visit(visitor);
        (self.hooks.visit_roots)(self, visitor);
    }

    pub(crate) fn visit_code_on_stack(&self, addresses: &[usize], visitor: &mut Visitor) {
        (self.hooks.visit_code_on_stack)(self, addresses, visitor);
    }

    pub(crate) fn visit_output_constraints(&self, visitor: &mut Visitor) {
        self.services.visit_output_constraints(visitor);
    }

    pub(crate) fn reconcile_weak_references(&self, survivors: &Survivors) {
        self.shape_cache.prune(survivors);
        self.string_caches.clear();
        self.number_strings.reconcile(survivors);
        self.symbols.prune(survivors);
        self.weakly_held_symbols.prune(survivors);
        self.native_executables.prune(survivors);
        self.custom_accessor_functions.prune(survivors);
        self.services.reconcile_weak_references(self, survivors);
        (self.hooks.reconcile_weak_references)(self, survivors);
        if let Some(reconcile) = self.embedder.reconcile_weak_references.get() {
            reconcile(self, survivors);
        }
    }

    pub(crate) fn did_collect(&self, scope: Scope, cells_on_stack: &[usize]) {
        self.string_caches.release_retired(cells_on_stack);
        self.invalidate_prototype_chain_caches();
        self.services.did_collect(self, scope);
        (self.hooks.did_collect)(self, scope);
        self.will_collect(scope);
    }
}

const _: () = {
    assert!(Executable::offset_of_entry(Specialization::Call) == size_of::<JsCell>());
    assert!(bun_engine_bytecode::CellType::STRING.0 == CellType::String.0);
    assert!(bun_engine_bytecode::CellType::BIGINT.0 == CellType::BigInt.0);
    assert!(bun_engine_bytecode::CellType::SYMBOL.0 == CellType::Symbol.0);
};
