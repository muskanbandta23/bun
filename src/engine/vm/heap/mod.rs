//! The garbage-collected heap.

mod activity;
mod allocator;
mod analyzer;
pub mod barrier;
mod block;
mod block_memory;
mod block_set;
mod cell;
mod collector;
mod conservative;
mod container;
mod directory;
mod field;
mod gc;
mod iteration;
mod marking;
mod once_box;
mod policy;
mod precise;
mod root_alloc;
mod space;
pub mod stack;
mod statistics;
mod strong;
mod sweeper;
mod trace;
mod value_vec;
mod verifier;
pub mod virtual_memory;
mod visitor;
mod weak;
mod weak_map;
mod weak_table;

use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bun_engine_macros::FieldOffsets;

pub use allocator::Allocator;
pub use analyzer::{HeapAnalyzer, HeapGraphSink, HeapObserver, MarkingConstraint, RootReason};
pub use block::{ATOM_SIZE, BLOCK_SIZE, Block, PAYLOAD_SIZE};
pub use block_memory::shape_base;
pub use cell::{CellState, JsCell};
pub use field::{
    Field, FieldArray, FieldValue, GcCell, GcRef, GcRefMut, Owned, OwnedField, PlainKey, TracedMap,
    TracedVec, WeakField,
};
pub(crate) use gc::keep_alive;
pub use gc::{CellKind, Extends, Gc, GcFree, Held};
pub use marking::is_helper_thread;
pub use once_box::OnceBox;
pub use policy::{HeapKind, HeapSizes};
pub use precise::{PRECISE_BIT, PreciseAllocation};
pub use root_alloc::{
    RootAlloc, free_root_words, try_allocate_root_words, RootBox, RootDeque, RootMap, RootSet, RootVec, root_box, root_vec,
};
pub use space::{
    EXACT_CUTOFF, IsoSpace, LARGE_CUTOFF, SIZE_CLASS_CHECKSUM, SIZE_CLASS_COUNT, SIZE_CLASS_OF_STEP, Space, optimal_size, size_class,
};
pub use statistics::TypeCounts;
pub use strong::{Strong, duplicate_strong_slot, free_strong_slot};
pub use trace::{Trace, destroy, select_trace, visit_children_of};
pub use value_vec::ValueVec;
pub use visitor::{Survivors, Visitor};
pub use weak::{Weak, WeakImpl, WeakOwner};
pub(crate) use weak_map::WeakValueMap;
pub use weak_table::WeakTable;

use crate::{FastHashMap, JsRopeString, JsString, Shape, Value, Vm, object, options};
use block::Version;
use block_memory::BlockSource;
use block_set::BlockSet;
use container::Container;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Scope {
    /// Only what has been allocated since the last collection can die.
    Eden,
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum MutatorState {
    Running,
    Sweeping,
    Collecting,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AllocationKind {
    Cell,
    /// Memory without a header, which the cell that owns it marks and traces.
    Auxiliary,
}

pub type DestroyFunction = unsafe extern "C" fn(*mut JsCell);
pub type Finalizer = unsafe extern "C" fn(cell: *mut JsCell, context: *mut c_void);

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Destruction {
    ByFunction,
    /// The sweeper knows the function.
    OfStrings,
    /// The same, but only in a block that has asked for it.
    OnRequest,
}

#[derive(Clone, Copy)]
pub(crate) struct CellAttributes {
    kind: AllocationKind,
    destruction: Destruction,
    destroy: Option<DestroyFunction>,
}

impl CellAttributes {
    const fn new(kind: AllocationKind, destroy: Option<DestroyFunction>) -> CellAttributes {
        CellAttributes {
            kind,
            destruction: Destruction::ByFunction,
            destroy,
        }
    }

    const fn cell(destroy: Option<DestroyFunction>) -> CellAttributes {
        Self::new(AllocationKind::Cell, destroy)
    }

    const fn string(destruction: Destruction) -> CellAttributes {
        CellAttributes {
            destruction,
            ..Self::cell(Some(destroy::<JsString>))
        }
    }

    fn destroys_every_cell(self) -> bool {
        self.destroy.is_some() && self.destruction != Destruction::OnRequest
    }
}

#[derive(FieldOffsets)]
#[repr(C)]
pub struct Heap {
    /// A store into a cell whose state is at most this takes the slow path of the write barrier.
    pub barrier_threshold: Cell<u32>,
    pub mutator_should_be_fenced: Cell<bool>,
    state: Cell<MutatorState>,
    did_defer_collection: Cell<bool>,
    deferral_depth: Cell<u32>,

    pub strings: IsoSpace,
    pub rope_strings: IsoSpace,
    pub shapes: IsoSpace,
    /// Cells that need no destruction.
    pub cells: Space,
    pub auxiliary: Space,
    /// Objects that have their class in a field.
    pub destructible_objects: Space,
    /// Cells whose shape lives as long as the heap.
    pub destructible_cells: Space,

    #[expect(clippy::vec_box, reason = "handed out by address")]
    iso_spaces: RefCell<Vec<Box<IsoSpace>>>,
    spaces_with_output_constraints: RefCell<Vec<&'static IsoSpace>>,
    allocators: RefCell<Vec<&'static Allocator>>,
    steal_cursor: Cell<usize>,
    sweeper: sweeper::Sweeper,
    /// A block is being swept.
    is_destroying_cells: Cell<bool>,
    blocks: RefCell<BlockSet>,
    precise: RefCell<Vec<&'static PreciseAllocation>>,
    /// The precise allocations from here on are new since the last collection.
    first_young_precise: Cell<usize>,
    precise_bytes: Cell<usize>,

    marking_version: AtomicU32,
    allocation_version: AtomicU32,
    is_marking_full: AtomicBool,
    collection_scope: Cell<Option<Scope>>,
    last_collection_scope: Cell<Option<Scope>>,
    is_idle_collection: Cell<bool>,
    collection_count: Cell<u64>,
    remembered: RefCell<Vec<usize>>,
    spare_mark_stack: Cell<Vec<usize>>,
    opaque_roots: marking::OpaqueRoots,
    analyzer: Cell<*mut HeapAnalyzer>,

    strong_set: strong::StrongSet,
    weak_set: weak::WeakSet,
    weak_tables: weak_table::WeakTables,
    protected: RefCell<FastHashMap<usize, u32>>,
    observers: RefCell<Vec<*const HeapObserver>>,
    constraints: RefCell<Vec<MarkingConstraint>>,

    secret: Cell<u64>,
    policy: policy::Policy,
    activity: activity::Activity,
}

pub(crate) struct StateGuard<'a> {
    heap: &'a Heap,
    previous: MutatorState,
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        self.heap.state.set(self.previous);
    }
}

struct DestructionGuard<'a>(StateGuard<'a>);

impl Drop for DestructionGuard<'_> {
    fn drop(&mut self) {
        self.0.heap.is_destroying_cells.set(false);
    }
}

/// No collection runs while it is alive. One that has become due runs at the next opportunity.
pub struct DeferCollection<'a>(&'a Heap);

impl Drop for DeferCollection<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.end_deferral();
    }
}

impl Heap {
    /// What a `Vm` sets aside for it.
    pub const SIZE_LIMIT: usize = 10240;

    pub(crate) fn new(kind: HeapKind, seed: u64) -> Heap {
        block_memory::initialize();
        let strings = |size, destruction| {
            let attributes = CellAttributes::string(destruction);
            IsoSpace::new(size, attributes, BlockSource::General)
        };
        let first_version = options().initial_heap_version.max(block::FIRST_VERSION);
        Heap {
            barrier_threshold: Cell::new(u32::from(barrier::BLACK_THRESHOLD)),
            mutator_should_be_fenced: Cell::new(false),
            state: Cell::new(MutatorState::Running),
            did_defer_collection: Cell::new(false),
            deferral_depth: Cell::new(0),
            strings: strings(size_of::<JsString>(), Destruction::OfStrings),
            rope_strings: strings(size_of::<JsRopeString>(), Destruction::OnRequest),
            shapes: IsoSpace::new(
                size_of::<Shape>(),
                CellAttributes::cell(Some(destroy::<Shape>)),
                BlockSource::Shapes,
            ),
            cells: Space::new(CellAttributes::cell(None)),
            auxiliary: Space::new(CellAttributes::new(AllocationKind::Auxiliary, None)),
            destructible_objects: Space::new(CellAttributes::cell(Some(
                object::destroy_by_class_field,
            ))),
            destructible_cells: Space::new(CellAttributes::cell(Some(object::destroy_by_shape))),
            iso_spaces: RefCell::default(),
            spaces_with_output_constraints: RefCell::default(),
            allocators: RefCell::default(),
            steal_cursor: Cell::new(0),
            sweeper: sweeper::Sweeper::default(),
            is_destroying_cells: Cell::new(false),
            blocks: RefCell::new(BlockSet::new()),
            precise: RefCell::default(),
            first_young_precise: Cell::new(0),
            precise_bytes: Cell::new(0),
            marking_version: AtomicU32::new(first_version),
            allocation_version: AtomicU32::new(first_version),
            is_marking_full: AtomicBool::new(false),
            collection_scope: Cell::new(None),
            last_collection_scope: Cell::new(None),
            is_idle_collection: Cell::new(false),
            collection_count: Cell::new(0),
            remembered: RefCell::default(),
            spare_mark_stack: Cell::default(),
            opaque_roots: marking::OpaqueRoots::default(),
            analyzer: Cell::new(core::ptr::null_mut()),
            strong_set: strong::StrongSet::default(),
            weak_set: weak::WeakSet::default(),
            weak_tables: weak_table::WeakTables::default(),
            protected: RefCell::default(),
            observers: RefCell::default(),
            constraints: RefCell::default(),
            secret: Cell::new(seed | 1),
            policy: policy::Policy::new(kind),
            activity: activity::Activity::default(),
        }
    }

    #[inline(always)]
    pub fn vm(&self) -> &Vm {
        let address = core::ptr::from_ref(self).addr() - core::mem::offset_of!(Vm, heap);
        // SAFETY: a heap exists only as that field of a `Vm`, whose provenance is exposed.
        unsafe { &*core::ptr::with_exposed_provenance(address) }
    }

    #[inline(always)]
    fn marking_version(&self) -> Version {
        self.marking_version.load(Ordering::Relaxed)
    }

    #[inline(always)]
    fn allocation_version(&self) -> Version {
        self.allocation_version.load(Ordering::Relaxed)
    }

    fn is_marking_full(&self) -> bool {
        self.is_marking_full.load(Ordering::Relaxed)
    }

    pub fn mutator_state(&self) -> MutatorState {
        self.state.get()
    }

    pub fn collection_scope(&self) -> Option<Scope> {
        self.collection_scope.get()
    }

    pub fn last_collection_scope(&self) -> Option<Scope> {
        self.last_collection_scope.get()
    }

    /// The one in progress has been asked for by `collect_when_idle`.
    pub fn is_idle_collection(&self) -> bool {
        self.is_idle_collection.get()
    }

    /// Changes whenever scripts may have run since marking last looked.
    pub fn mutator_execution_version(&self) -> u64 {
        self.collection_count.get()
    }

    fn enter_destruction(&self) -> DestructionGuard<'_> {
        assert!(!self.is_destroying_cells.replace(true));
        DestructionGuard(self.enter_state(MutatorState::Sweeping))
    }

    pub(crate) fn enter_state(&self, state: MutatorState) -> StateGuard<'_> {
        StateGuard {
            heap: self,
            previous: self.state.replace(state),
        }
    }

    fn next_secret(&self) -> u64 {
        let mut state = self.secret.get();
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.secret.set(state);
        state
    }

    fn register_allocator(&self, allocator: &Allocator) {
        // SAFETY: a part of the heap or of a space that it owns, which does not move.
        let allocator = unsafe { &*core::ptr::from_ref(allocator) };
        self.allocators.borrow_mut().push(allocator);
    }

    fn for_each_directory(&self, mut visit: impl FnMut(&directory::Directory)) {
        let mut index = 0;
        loop {
            let Some(&allocator) = self.allocators.borrow().get(index) else {
                return;
            };
            allocator.directory().into_iter().for_each(&mut visit);
            index += 1;
        }
    }

    pub fn create_iso_space(&self, size: usize, destroy: Option<DestroyFunction>) -> &IsoSpace {
        let attributes = CellAttributes::cell(destroy);
        let space = Box::new(IsoSpace::new(size, attributes, BlockSource::General));
        let address = core::ptr::from_ref(&*space);
        self.iso_spaces.borrow_mut().push(space);
        // SAFETY: boxed, and kept until the heap goes.
        unsafe { &*address }
    }

    /// The cells get references without a barrier, which `visit_output_constraints` shows. Panics unless the heap made
    /// the space.
    pub fn constrain_by_cells_in(&self, space: &IsoSpace) {
        assert!((self.iso_spaces.borrow().iter()).any(|it| core::ptr::eq(&raw const **it, space)));
        // SAFETY: lives as long as the heap.
        let space = unsafe { &*core::ptr::from_ref(space) };
        self.spaces_with_output_constraints.borrow_mut().push(space);
    }

    fn steal_block(&self) -> Option<usize> {
        loop {
            let cursor = self.steal_cursor.get();
            let allocator = *self.allocators.borrow().get(cursor)?;
            if allocator.source == BlockSource::General
                && let Some(block) = allocator.directory().and_then(|it| it.take_empty_block())
            {
                return Some(block);
            }
            self.steal_cursor.set(cursor + 1);
        }
    }

    /// In a block of the directory, in an empty one of another, or in a new one.
    fn find_free_cells(
        &self,
        directory: &directory::Directory,
        secret: u64,
    ) -> Option<(&'static Block, block::FreeList)> {
        let source = directory.allocator().source;
        let mut memory = None;
        // A finalizer that has yet to run counts on the dead being as they were.
        if self.mutator_state() == MutatorState::Running {
            if let found @ Some(_) = directory.find_free_cells(secret) {
                return found;
            }
            if source == BlockSource::General {
                memory = self.steal_block();
            }
        }
        let memory = memory.or_else(|| {
            let block = source.allocate()?;
            self.blocks.borrow_mut().add(block);
            Some(block)
        })?;
        // SAFETY: empty, and in no directory.
        Some(unsafe { directory.add_block(memory, secret) })
    }

    /// For a cell of a space whose cells do not all need it.
    #[inline]
    pub(crate) fn request_destruction(cell: &JsCell) {
        if let Container::Block(block) = container::of(cell.address())
            && block.request_destruction()
        {
            block.directory().did_request_destruction(block);
        }
    }

    #[inline(never)]
    fn allocate_precisely(
        &self,
        size: usize,
        attributes: CellAttributes,
        allocate: fn(usize) -> *mut c_void,
    ) -> usize {
        self.will_allocate_slowly();
        let create = || PreciseAllocation::try_create(self, size, attributes, allocate);
        let could_fit = size < options().ram_size;
        let Some(allocation) = create()
            .or_else(|| (could_fit && self.collect_for_lack_of_memory()).then(create).flatten())
        else {
            return 0;
        };
        let mut precise = self.precise.borrow_mut();
        allocation.index.set(precise.len() as u32);
        precise.push(allocation);
        self.precise_bytes.update(|bytes| bytes + size);
        self.did_allocate(size);
        allocation.cell()
    }

    /// Makes auxiliary memory that has `PRECISE_BIT` larger. The new address, or zero if it stays as it is. May
    /// collect.
    ///
    /// # Safety
    /// Its owner is alive and is all that has the address, which it replaces before the next allocation.
    pub(crate) unsafe fn try_grow_precise(&self, cell: usize, size: usize) -> usize {
        self.collect_if_necessary();
        if barrier::is_fenced() || self.mutator_state() != MutatorState::Running {
            return 0;
        }
        // SAFETY: the caller's.
        let (old_size, resized) = unsafe {
            let old_size = PreciseAllocation::of_cell(cell).cell_size();
            (old_size, PreciseAllocation::try_resize(cell, size))
        };
        let Some(allocation) = resized else { return 0 };
        self.precise.borrow_mut()[allocation.index.get() as usize] = allocation;
        self.precise_bytes.update(|bytes| bytes + size - old_size);
        self.did_allocate(size - old_size);
        if allocation.is_marked() {
            self.policy.did_grow_old_cell(size - old_size);
        }
        allocation.cell()
    }

    fn remember(&self, cell: &JsCell) {
        cell.set_state(CellState::PossiblyGrey);
        self.remembered
            .borrow_mut()
            .push(cell.address() | visitor::REVISIT);
    }

    #[inline]
    pub fn defer_collection(&self) -> DeferCollection<'_> {
        self.begin_deferral();
        DeferCollection(self)
    }

    #[inline]
    pub fn begin_deferral(&self) {
        self.deferral_depth.update(|depth| depth + 1);
    }

    #[inline]
    pub fn end_deferral(&self) {
        self.deferral_depth.update(|depth| depth - 1);
    }

    /// For a caller that can stand a collection.
    pub fn collect_if_deferred(&self) {
        if self.deferral_depth.get() == 0 && self.did_defer_collection.take() {
            self.collect_if_necessary();
        }
    }

    #[inline]
    pub fn is_deferred(&self) -> bool {
        self.deferral_depth.get() != 0
    }

    #[inline]
    pub fn allocate_strong_slot(&self, word: u64) -> usize {
        self.strong_set.allocate(word)
    }

    /// # Safety
    /// As for `Weak::with_owner`.
    pub unsafe fn allocate_weak_handle(
        &self,
        cell: Gc<JsCell>,
        owner: *const WeakOwner,
        context: *mut c_void,
    ) -> &'static WeakImpl {
        self.weak_set.allocate(self, cell.address(), owner, context)
    }

    pub fn protect(&self, value: Value) {
        if let Some(cell) = value.as_cell() {
            *self
                .protected
                .borrow_mut()
                .entry(cell.address())
                .or_default() += 1;
        }
    }

    /// Whether that was the last protection.
    pub fn unprotect(&self, value: Value) -> bool {
        let address = value.to_bits() as usize;
        let mut protected = self.protected.borrow_mut();
        match protected.get_mut(&address) {
            Some(1) => protected.remove(&address).is_some(),
            Some(count) => {
                *count -= 1;
                false
            }
            None => false,
        }
    }

    /// # Safety
    /// `finalizer` can be called with the dead cell and `context`.
    pub unsafe fn add_finalizer(
        &self,
        cell: Gc<JsCell>,
        finalizer: Finalizer,
        context: *mut c_void,
    ) {
        unsafe extern "C" fn is_reachable(
            _: &WeakOwner,
            _: &WeakImpl,
            _: *mut c_void,
            _: &mut Visitor,
            _: Option<&mut crate::AsciiLiteral>,
        ) -> bool {
            false
        }
        unsafe extern "C" fn finalize(
            _: &WeakOwner,
            handle: &WeakImpl,
            boxed: *mut c_void,
        ) -> bool {
            // SAFETY: the box below, and the promise of its maker.
            unsafe {
                let (finalizer, context) = *Box::from_raw(boxed.cast::<(Finalizer, *mut c_void)>());
                finalizer(
                    core::ptr::with_exposed_provenance_mut(handle.target()),
                    context,
                );
            }
            true
        }
        static OWNER: WeakOwner = WeakOwner {
            is_reachable_from_opaque_roots: is_reachable,
            finalize,
        };
        let boxed = Box::into_raw(Box::new((finalizer, context)));
        self.weak_set
            .allocate(self, cell.address(), &raw const OWNER, boxed.cast());
    }

    /// # Safety
    /// The observer stays valid until it is removed.
    pub unsafe fn add_observer(&self, observer: *const HeapObserver) {
        self.observers.borrow_mut().push(observer);
    }

    pub fn remove_observer(&self, observer: *const HeapObserver) {
        self.observers.borrow_mut().retain(|&it| it != observer);
    }

    pub fn add_marking_constraint(&self, constraint: MarkingConstraint) {
        self.constraints.borrow_mut().push(constraint);
    }

    /// False for a cell that the last collection found dead and that has not been swept.
    ///
    /// # Safety
    /// The address of a cell of this heap whose memory has not been freed.
    pub unsafe fn is_live(&self, cell: usize) -> bool {
        match container::of(cell) {
            Container::Precise(allocation) => allocation.is_live(),
            Container::Block(block) if block.is_free_listed() => {
                !block.directory().allocator().is_free(cell)
            }
            Container::Block(block) => block.is_live(cell),
        }
    }

    fn stop_allocating(&self) {
        for allocator in self.allocators.borrow().iter() {
            allocator.stop_allocating(self);
        }
    }

    /// Destroys every cell and frees all memory.
    ///
    /// # Safety
    /// Nothing uses a cell afterwards.
    pub(crate) unsafe fn tear_down(&self) {
        let _state = self.enter_state(MutatorState::Sweeping);
        self.stop_allocating();
        self.weak_set.finalize_all();
        self.stop_allocating();
        self.for_each_directory(|directory| {
            (directory.blocks().iter()).for_each(|block| block.forget_liveness());
            directory.begin_full_marking();
            directory.end_marking(Scope::Full);
        });
        let precise = self.precise.take();
        for allocation in &precise {
            allocation.begin_full_marking();
            allocation.end_marking();
        }
        // A destructor may look at another dead cell, and at its shape.
        let sweep = |source| {
            self.for_each_directory(|directory| {
                while directory.allocator().source == source && directory.sweep_next_block() {}
            });
        };
        sweep(BlockSource::General);
        // SAFETY: the caller's.
        precise.iter().for_each(|it| unsafe { it.destroy_cell() });
        sweep(BlockSource::Shapes);
        // SAFETY: as above.
        precise.iter().for_each(|it| unsafe { it.free() });
        self.free_swept_empty_blocks(0);
        assert!(self.blocks.borrow().len() == 0);
    }
}
