//! The garbage-collected heap.

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
mod marking;
mod once_box;
mod policy;
mod precise;
mod root_alloc;
mod space;
pub mod stack;
mod strong;
mod trace;
mod value_vec;
pub mod virtual_memory;
mod visitor;
mod weak;
mod weak_map;

use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use bun_threading::Guarded;

pub use allocator::Allocator;
pub use analyzer::{HeapAnalyzer, HeapGraphSink, HeapObserver, MarkingConstraint, RootReason};
pub use block::{ATOM_SIZE, BLOCK_SIZE, Block};
pub use block_memory::shape_base;
pub use cell::{CellState, JsCell};
pub use field::{
    Field, FieldArray, FieldValue, GcCell, GcRef, GcRefMut, Owned, OwnedField, PlainKey, TracedMap,
    TracedVec, WeakField,
};
pub(crate) use gc::keep_alive;
pub use gc::{CellKind, Extends, Gc, GcFree};
pub use once_box::OnceBox;
pub use policy::{HeapKind, HeapSizes};
pub use precise::{PRECISE_BIT, PreciseAllocation};
pub use root_alloc::{
    RootAlloc, RootBox, RootDeque, RootMap, RootSet, RootVec, root_box, root_vec,
};
pub use space::{
    IsoSpace, LARGE_CUTOFF, SIZE_CLASS_COUNT, SIZE_CLASS_OF_STEP, Space, optimal_size, size_class,
};
pub use strong::{Strong, free_strong_slot};
pub use trace::{Trace, destroy, select_trace, visit_children_of};
pub use value_vec::ValueVec;
pub use visitor::{Survivors, Visitor};
pub use weak::{Weak, WeakImpl, WeakOwner};
pub(crate) use weak_map::WeakValueMap;

use crate::{FastHashMap, FastHashSet, JsRopeString, JsString, Shape, Value, Vm, object};
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

#[derive(Clone, Copy)]
pub(crate) struct CellAttributes {
    kind: AllocationKind,
    destroy: Option<DestroyFunction>,
}

impl CellAttributes {
    const fn cell(destroy: Option<DestroyFunction>) -> CellAttributes {
        CellAttributes {
            kind: AllocationKind::Cell,
            destroy,
        }
    }
}

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
    allocators: RefCell<Vec<&'static Allocator>>,
    steal_cursor: Cell<usize>,
    sweep_cursor: Cell<usize>,
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
    remembered: RefCell<Vec<usize>>,
    spare_mark_stack: Cell<Vec<usize>>,
    opaque_roots: Guarded<FastHashSet<usize>>,
    analyzer: Cell<*mut HeapAnalyzer>,

    strong_set: strong::StrongSet,
    weak_set: weak::WeakSet,
    protected: RefCell<FastHashMap<usize, u32>>,
    observers: RefCell<Vec<*const HeapObserver>>,
    constraints: RefCell<Vec<MarkingConstraint>>,

    secret: Cell<u64>,
    policy: policy::Policy,
    #[cfg(debug_assertions)]
    stress_countdown: Cell<u32>,
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

/// No collection runs while it is alive. One that has become due runs at the next opportunity.
pub struct DeferCollection<'a>(&'a Heap);

impl Drop for DeferCollection<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.end_deferral();
    }
}

impl Heap {
    pub(crate) fn new(kind: HeapKind, seed: u64) -> Heap {
        block_memory::initialize();
        let iso = |size, destroy, source| {
            IsoSpace::new(size, CellAttributes::cell(Some(destroy)), source)
        };
        Heap {
            barrier_threshold: Cell::new(u32::from(barrier::BLACK_THRESHOLD)),
            mutator_should_be_fenced: Cell::new(false),
            state: Cell::new(MutatorState::Running),
            did_defer_collection: Cell::new(false),
            deferral_depth: Cell::new(0),
            strings: iso(
                size_of::<JsString>(),
                destroy::<JsString>,
                BlockSource::General,
            ),
            rope_strings: iso(
                size_of::<JsRopeString>(),
                destroy::<JsString>,
                BlockSource::General,
            ),
            shapes: iso(size_of::<Shape>(), destroy::<Shape>, BlockSource::Shapes),
            cells: Space::new(CellAttributes::cell(None)),
            auxiliary: Space::new(CellAttributes {
                kind: AllocationKind::Auxiliary,
                destroy: None,
            }),
            destructible_objects: Space::new(CellAttributes::cell(Some(
                object::destroy_by_class_field,
            ))),
            destructible_cells: Space::new(CellAttributes::cell(Some(object::destroy_by_shape))),
            iso_spaces: RefCell::default(),
            allocators: RefCell::default(),
            steal_cursor: Cell::new(0),
            sweep_cursor: Cell::new(0),
            blocks: RefCell::new(BlockSet::new()),
            precise: RefCell::default(),
            first_young_precise: Cell::new(0),
            precise_bytes: Cell::new(0),
            marking_version: AtomicU32::new(block::FIRST_VERSION),
            allocation_version: AtomicU32::new(block::FIRST_VERSION),
            is_marking_full: AtomicBool::new(false),
            collection_scope: Cell::new(None),
            last_collection_scope: Cell::new(None),
            remembered: RefCell::default(),
            spare_mark_stack: Cell::default(),
            opaque_roots: Guarded::default(),
            analyzer: Cell::new(core::ptr::null_mut()),
            strong_set: strong::StrongSet::default(),
            weak_set: weak::WeakSet::default(),
            protected: RefCell::default(),
            observers: RefCell::default(),
            constraints: RefCell::default(),
            secret: Cell::new(seed | 1),
            policy: policy::Policy::new(kind),
            #[cfg(debug_assertions)]
            stress_countdown: Cell::new(0),
        }
    }

    #[inline(always)]
    pub(crate) fn vm(&self) -> &Vm {
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

    fn active_analyzer(&self) -> *mut HeapAnalyzer {
        self.analyzer.get()
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

    /// An empty block of another allocator, or a new one.
    fn acquire_block(&self, source: BlockSource) -> Option<usize> {
        if source == BlockSource::General
            && let Some(block) = self.steal_block()
        {
            return Some(block);
        }
        let block = source.allocate()?;
        self.blocks.borrow_mut().add(block);
        Some(block)
    }

    #[inline(never)]
    fn allocate_precisely(&self, size: usize, attributes: CellAttributes) -> usize {
        self.collect_if_necessary();
        let Some(allocation) = PreciseAllocation::try_create(self, size, attributes) else {
            return 0;
        };
        self.precise.borrow_mut().push(allocation);
        self.precise_bytes.update(|bytes| bytes + size);
        self.did_allocate(size);
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
        self.weak_set.allocate(cell.address(), owner, context)
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

    pub fn for_each_protected_cell(&self, mut visit: impl FnMut(Gc<JsCell>)) {
        let mut cells: FastHashSet<usize> = self.protected.borrow().keys().copied().collect();
        self.strong_set.for_each_cell(|cell| {
            cells.insert(cell);
        });
        let _defer = self.defer_collection();
        for cell in cells {
            // SAFETY: a root.
            visit(unsafe { Gc::from_address(cell) });
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
            .allocate(cell.address(), &raw const OWNER, boxed.cast());
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

    /// `visit` does not allocate.
    pub fn for_each_live_cell(&self, mut visit: impl FnMut(Gc<JsCell>)) {
        self.stop_allocating();
        let _state = self.enter_state(MutatorState::Collecting);
        self.for_each_directory(|directory| Self::live_cells_of(directory, &mut visit));
        let precise = self.precise.borrow().clone();
        for allocation in precise {
            if allocation.attributes.kind == AllocationKind::Cell && allocation.is_live() {
                // SAFETY: live.
                visit(unsafe { Gc::from_address(allocation.cell()) });
            }
        }
    }

    /// `visit` does not allocate.
    pub fn for_each_live_cell_in(&self, space: &IsoSpace, mut visit: impl FnMut(Gc<JsCell>)) {
        space.allocator.stop_allocating(self);
        let _state = self.enter_state(MutatorState::Collecting);
        if let Some(directory) = space.allocator.directory() {
            Self::live_cells_of(directory, &mut visit);
        }
    }

    fn live_cells_of(directory: &directory::Directory, visit: &mut impl FnMut(Gc<JsCell>)) {
        if directory.attributes().kind != AllocationKind::Cell {
            return;
        }
        for block in directory.blocks() {
            for cell in block.cells().filter(|&cell| block.is_live(cell)) {
                // SAFETY: live.
                visit(unsafe { Gc::from_address(cell) });
            }
        }
    }

    /// For a marking constraint.
    pub fn for_each_marked_cell_in(&self, space: &IsoSpace, mut visit: impl FnMut(Gc<JsCell>)) {
        let version = self.marking_version();
        let blocks = space.allocator.directory();
        for block in blocks.into_iter().flat_map(|it| it.blocks_with_marks()) {
            for cell in block.cells().filter(|&cell| block.is_marked(version, cell)) {
                // SAFETY: marked.
                visit(unsafe { Gc::from_address(cell) });
            }
        }
    }

    /// False if nothing is left to sweep.
    pub fn sweep_next_block(&self) -> bool {
        loop {
            let cursor = self.sweep_cursor.get();
            let Some(&allocator) = self.allocators.borrow().get(cursor) else {
                return false;
            };
            if allocator
                .directory()
                .is_some_and(|it| it.sweep_next_block())
            {
                return true;
            }
            self.sweep_cursor.set(cursor + 1);
        }
    }

    pub fn sweep_synchronously(&self) {
        while self.sweep_next_block() {}
        self.free_empty_blocks(0);
    }

    /// Gives the memory of all but `keep` empty blocks back.
    pub fn free_empty_blocks(&self, keep: usize) {
        let mut empty = 0;
        self.for_each_directory(|directory| empty += directory.empty_block_count());
        let mut excess = empty.saturating_sub(keep);
        self.for_each_directory(|directory| {
            while excess != 0
                && let Some(block) = directory.take_empty_block()
            {
                self.blocks.borrow_mut().remove(block);
                // SAFETY: empty, and in no directory.
                unsafe { directory.allocator().source.free(block) };
                excess -= 1;
            }
        });
    }

    /// Destroys every cell and frees all memory.
    ///
    /// # Safety
    /// Nothing uses a cell afterwards.
    pub(crate) unsafe fn tear_down(&self) {
        self.stop_allocating();
        let _state = self.enter_state(MutatorState::Sweeping);
        self.weak_set.finalize_all();
        for allocation in self.precise.take() {
            // SAFETY: the caller's.
            unsafe { allocation.destroy() };
        }
        // Destroying a cell may read its shape.
        for source in [BlockSource::General, BlockSource::Shapes] {
            self.for_each_directory(|directory| {
                if directory.allocator().source == source {
                    directory
                        .blocks()
                        .iter()
                        .for_each(|it| it.forget_liveness());
                    directory.begin_full_marking();
                    directory.end_marking(Scope::Full);
                    while directory.sweep_next_block() {}
                }
            });
        }
        self.free_empty_blocks(0);
    }
}
