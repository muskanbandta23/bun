use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering, fence};

use bun_engine_macros::FieldOffsets;
use bun_threading::Guarded;

use super::{Gc, GcFree, Heap, Scope, Survivors, Visitor};
use crate::{AsciiLiteral, CellKind, Vm};

/// Decides whether the target of a weak handle lives on, and hears of its death.
#[repr(C, align(8))]
pub struct WeakOwner {
    pub is_reachable_from_opaque_roots: unsafe extern "C" fn(
        owner: &WeakOwner,
        handle: &WeakImpl,
        context: *mut c_void,
        visitor: &mut Visitor,
        reason: Option<&mut AsciiLiteral>,
    ) -> bool,
    /// Runs before the target is destroyed. True: nothing else refers to the handle, so free it.
    pub finalize:
        unsafe extern "C" fn(owner: &WeakOwner, handle: &WeakImpl, context: *mut c_void) -> bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum State {
    Live = 0,
    Dead = 1,
    Finalized = 2,
    /// Its holder has let go of it, or it has none yet.
    Deallocated = 3,
}

const STATE_MASK: usize = 3;

#[derive(FieldOffsets)]
#[repr(C)]
pub struct WeakImpl {
    /// In a handle that waits to be allocated, the address of the next one.
    cell: Cell<usize>,
    /// Its holder may let go of it on another thread.
    owner_and_state: AtomicUsize,
    context: Cell<*mut c_void>,
}

impl WeakImpl {
    #[inline(always)]
    fn state(&self) -> State {
        match self.owner_and_state.load(Ordering::Relaxed) & STATE_MASK {
            0 => State::Live,
            1 => State::Dead,
            2 => State::Finalized,
            _ => State::Deallocated,
        }
    }

    /// False if it was in another state.
    fn change_state(&self, from: State, to: State) -> bool {
        let owner = self.owner_and_state.load(Ordering::Relaxed) & !STATE_MASK;
        let (from, to) = (owner | from as usize, owner | to as usize);
        self.owner_and_state
            .compare_exchange(from, to, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }

    fn owner(&self) -> Option<&WeakOwner> {
        let address = self.owner_and_state.load(Ordering::Relaxed) & !STATE_MASK;
        // SAFETY: an owner outlives its handles.
        unsafe { core::ptr::with_exposed_provenance::<WeakOwner>(address).as_ref() }
    }

    /// The address of the target, also once it is dead.
    pub fn target(&self) -> usize {
        self.cell.get()
    }

    /// The memory is reclaimed later.
    #[inline]
    pub fn deallocate(&self) {
        self.owner_and_state
            .store(State::Deallocated as usize, Ordering::Release);
    }

    /// On any thread.
    pub(super) fn visit(&self, visitor: &mut Visitor) {
        let (State::Live, Some(owner)) = (self.state(), self.owner()) else {
            return;
        };
        if visitor.is_address_marked(self.target()) {
            return;
        }
        let mut reason = AsciiLiteral::NULL;
        let wants_reason = visitor.analyzer().is_some();
        // SAFETY: the target has not been destroyed.
        let is_reachable = unsafe {
            (owner.is_reachable_from_opaque_roots)(
                owner,
                self,
                self.context.get(),
                visitor,
                wants_reason.then_some(&mut reason),
            )
        };
        if is_reachable {
            // SAFETY: as above.
            unsafe { visitor.append_word(self.target() as u64) };
            if let Some(analyzer) = visitor.analyzer() {
                analyzer.opaque_root_reason(self.target(), reason);
            }
        }
    }

    /// Whether `finalize` has to follow.
    fn did_die(&self) -> bool {
        if self.owner().is_none() {
            self.change_state(State::Live, State::Finalized);
            return false;
        }
        self.change_state(State::Live, State::Dead)
    }

    fn finalize(&self) {
        if let Some(owner) = self.owner()
            && self.change_state(State::Dead, State::Finalized)
            // SAFETY: nothing has been swept since the target died.
            && unsafe { (owner.finalize)(owner, self, self.context.get()) }
        {
            self.deallocate();
        }
    }
}

const HANDLES_PER_CHUNK: usize = 170;

struct Chunk([WeakImpl; HANDLES_PER_CHUNK]);

impl Chunk {
    fn deallocated(&self) -> impl DoubleEndedIterator<Item = &WeakImpl> {
        self.0.iter().filter(|it| it.state() == State::Deallocated)
    }
}

/// Owns the chunk, but hands out references that outlive a borrow.
struct ChunkBox(NonNull<Chunk>);

// SAFETY: one that leaves its thread has no live handle, and only the states are looked at.
unsafe impl Send for ChunkBox {}

impl ChunkBox {
    fn new() -> ChunkBox {
        let handles = [const {
            WeakImpl {
                cell: Cell::new(0),
                owner_and_state: AtomicUsize::new(State::Deallocated as usize),
                context: Cell::new(core::ptr::null_mut()),
            }
        }; HANDLES_PER_CHUNK];
        ChunkBox(NonNull::from(Box::leak(Box::new(Chunk(handles)))))
    }

    fn get(&self) -> &'static Chunk {
        // SAFETY: freed only when no handle is held, and no list of the set has one.
        unsafe { self.0.as_ref() }
    }

    fn is_unused(&self) -> bool {
        self.get().deallocated().count() == HANDLES_PER_CHUNK
    }
}

impl Drop for ChunkBox {
    fn drop(&mut self) {
        assert!(self.is_unused());
        fence(Ordering::Acquire);
        // SAFETY: leaked by `new`, and those who held the handles have let go.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// Those with handles that were still held when their heap went.
static ORPHANS: Guarded<Vec<ChunkBox>> = Guarded::new(Vec::new());

#[derive(Default)]
pub(super) struct WeakSet {
    first_free: Cell<usize>,
    /// Allocated from since the last collection. The free handles are in the last one.
    young: RefCell<Vec<ChunkBox>>,
    /// Had room when a collection last looked.
    available: RefCell<Vec<ChunkBox>>,
    full: RefCell<Vec<ChunkBox>>,
    /// What the collection in progress looks at. Nothing is allocated from them meanwhile.
    examined: RefCell<Vec<ChunkBox>>,
    dead: RefCell<Vec<&'static WeakImpl>>,
}

impl WeakSet {
    /// With fewer free handles than that, a chunk counts as full.
    const MIN_FREE: usize = HANDLES_PER_CHUNK / 8;

    #[inline]
    pub(super) fn allocate(
        &self,
        heap: &Heap,
        cell: usize,
        owner: *const WeakOwner,
        context: *mut c_void,
    ) -> &'static WeakImpl {
        if self.first_free.get() == 0 {
            self.refill(heap);
        }
        // SAFETY: chained by `refill`.
        let handle: &WeakImpl =
            unsafe { &*core::ptr::with_exposed_provenance(self.first_free.get()) };
        self.first_free.set(handle.cell.replace(cell));
        handle.context.set(context);
        let word = owner.expose_provenance() | State::Live as usize;
        handle.owner_and_state.store(word, Ordering::Relaxed);
        handle
    }

    #[inline(never)]
    fn refill(&self, heap: &Heap) {
        let mut young = self.young.borrow_mut();
        // Many handles do not last.
        if young
            .last()
            .is_none_or(|chunk| chunk.get().deallocated().count() < Self::MIN_FREE)
        {
            young.push(self.available.borrow_mut().pop().unwrap_or_else(|| {
                heap.did_allocate(size_of::<Chunk>());
                ChunkBox::new()
            }));
        }
        fence(Ordering::Acquire);
        for handle in young[young.len() - 1].get().deallocated().rev() {
            let address = core::ptr::from_ref(handle).expose_provenance();
            handle.cell.set(self.first_free.replace(address));
        }
    }

    pub(super) fn begin_marking(&self, scope: Scope) {
        self.first_free.set(0);
        let mut examined = self.examined.borrow_mut();
        examined.append(&mut self.young.borrow_mut());
        if scope == Scope::Full {
            examined.append(&mut self.available.borrow_mut());
            examined.append(&mut self.full.borrow_mut());
        }
    }

    fn live_handles(chunks: &[ChunkBox]) -> impl Iterator<Item = &'static WeakImpl> {
        let handles = chunks.iter().flat_map(|chunk| &chunk.get().0);
        handles.filter(|handle| handle.state() == State::Live)
    }

    /// Of those that a collection of that scope looks at.
    pub(super) fn handles_with_owner(&self, scope: Scope) -> Vec<&'static WeakImpl> {
        let mut handles = Vec::new();
        let mut add = |chunks: &RefCell<Vec<ChunkBox>>| {
            handles.extend(Self::live_handles(&chunks.borrow()).filter(|it| it.owner().is_some()));
        };
        add(&self.examined);
        if scope == Scope::Full {
            add(&self.available);
            add(&self.full);
        }
        handles
    }

    pub(super) fn reap(&self, survivors: &Survivors) {
        let examined = self.examined.borrow();
        let dead = Self::live_handles(&examined)
            .filter(|handle| !survivors.contains_address(handle.target()) && handle.did_die());
        self.dead.borrow_mut().extend(dead);
    }

    pub(super) fn finalize_dead(&self) {
        for handle in self.dead.take() {
            handle.finalize();
        }
        const SPARE_CHUNKS: usize = 4;
        let (mut available, mut full) = (self.available.borrow_mut(), self.full.borrow_mut());
        for chunk in self.examined.take() {
            match chunk.get().deallocated().count() {
                HANDLES_PER_CHUNK if available.len() >= SPARE_CHUNKS => drop(chunk),
                ..Self::MIN_FREE => full.push(chunk),
                _ => available.push(chunk),
            }
        }
    }

    /// The heap is going away with everything in it.
    pub(super) fn finalize_all(&self) {
        loop {
            self.begin_marking(Scope::Full);
            let examined = self.examined.borrow();
            let dead = Self::live_handles(&examined).filter(|handle| handle.did_die());
            self.dead.borrow_mut().extend(dead);
            drop(examined);
            self.finalize_dead();
            if self.young.borrow().is_empty() {
                return;
            }
        }
    }
}

impl Drop for WeakSet {
    fn drop(&mut self) {
        let mut orphans = ORPHANS.lock();
        for chunks in [&mut self.young, &mut self.available, &mut self.full] {
            orphans.append(chunks.get_mut());
        }
        orphans.retain(|chunk| !chunk.is_unused());
    }
}

/// Refers to a cell from anywhere in memory without keeping it alive.
#[repr(transparent)]
pub struct Weak<T> {
    handle: &'static WeakImpl,
    target: PhantomData<*const T>,
}

// SAFETY: the collector clears the handle.
unsafe impl<T> GcFree for Weak<T> {}

impl<T: CellKind> Weak<T> {
    pub fn new(vm: &Vm, cell: Gc<T>) -> Weak<T> {
        let (owner, context) = (core::ptr::null(), core::ptr::null_mut());
        // SAFETY: there is no owner.
        unsafe { Self::with_owner(vm, cell, owner, context) }
    }

    /// # Safety
    /// `owner` is null or outlives the handle, and can be called with `context`.
    pub unsafe fn with_owner(
        vm: &Vm,
        cell: Gc<T>,
        owner: *const WeakOwner,
        context: *mut c_void,
    ) -> Weak<T> {
        Weak {
            handle: (vm.heap.weak_set).allocate(&vm.heap, cell.address(), owner, context),
            target: PhantomData,
        }
    }

    #[inline]
    pub fn get(&self, _: &Vm) -> Option<Gc<T>> {
        // SAFETY: the state changes before the target is freed.
        self.is_live()
            .then(|| unsafe { Gc::from_address(self.handle.target()) })
    }

    #[inline]
    pub fn is_live(&self) -> bool {
        self.handle.state() == State::Live
    }

    pub fn into_raw(self) -> &'static WeakImpl {
        core::mem::ManuallyDrop::new(self).handle
    }
}

impl<T> Drop for Weak<T> {
    fn drop(&mut self) {
        self.handle.deallocate();
    }
}
