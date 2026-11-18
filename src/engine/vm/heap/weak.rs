use core::cell::{Cell, RefCell};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::{Gc, GcFree, Scope, Survivors, Visitor, container};
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
    /// Its holder has let go of it.
    Deallocated = 3,
}

const STATE_MASK: usize = 3;

#[repr(C)]
pub struct WeakImpl {
    cell: Cell<usize>,
    owner_and_state: Cell<usize>,
    context: Cell<*mut c_void>,
}

impl WeakImpl {
    #[inline(always)]
    fn state(&self) -> State {
        match self.owner_and_state.get() & STATE_MASK {
            0 => State::Live,
            1 => State::Dead,
            2 => State::Finalized,
            _ => State::Deallocated,
        }
    }

    fn set_state(&self, state: State) {
        self.owner_and_state
            .update(|word| word & !STATE_MASK | state as usize);
    }

    fn owner(&self) -> Option<&WeakOwner> {
        let address = self.owner_and_state.get() & !STATE_MASK;
        // SAFETY: an owner outlives its handles.
        unsafe { core::ptr::with_exposed_provenance::<WeakOwner>(address).as_ref() }
    }

    /// The address of the target, also once it is dead.
    pub fn target(&self) -> usize {
        self.cell.get()
    }

    /// The memory is reclaimed by the next collection that looks at the handle.
    #[inline]
    pub fn deallocate(&self) {
        self.set_state(State::Deallocated);
    }
}

const HANDLES_PER_CHUNK: usize = 168;

struct Chunk([WeakImpl; HANDLES_PER_CHUNK]);

#[derive(Default)]
pub(super) struct WeakSet {
    chunks: RefCell<Vec<NonNull<Chunk>>>,
    free: RefCell<Vec<&'static WeakImpl>>,
    /// Made since the last collection.
    young: RefCell<Vec<&'static WeakImpl>>,
    old: RefCell<Vec<&'static WeakImpl>>,
    /// Have an owner, and a target that marking has not reached.
    undecided: RefCell<Vec<&'static WeakImpl>>,
    dead: RefCell<Vec<&'static WeakImpl>>,
}

impl WeakSet {
    pub(super) fn allocate(
        &self,
        cell: usize,
        owner: *const WeakOwner,
        context: *mut c_void,
    ) -> &'static WeakImpl {
        let handle = self.free.borrow_mut().pop();
        let handle = handle.unwrap_or_else(|| self.grow());
        handle.cell.set(cell);
        handle
            .owner_and_state
            .set(owner.expose_provenance() | State::Live as usize);
        handle.context.set(context);
        self.young.borrow_mut().push(handle);
        handle
    }

    #[cold]
    fn grow(&self) -> &'static WeakImpl {
        let chunk: &'static Chunk = Box::leak(Box::new(Chunk(
            [const {
                WeakImpl {
                    cell: Cell::new(0),
                    owner_and_state: Cell::new(State::Deallocated as usize),
                    context: Cell::new(core::ptr::null_mut()),
                }
            }; HANDLES_PER_CHUNK],
        )));
        self.chunks.borrow_mut().push(NonNull::from(chunk));
        self.free.borrow_mut().extend(chunk.0[1..].iter().rev());
        &chunk.0[0]
    }

    pub(super) fn begin_marking(&self, scope: Scope) {
        if scope == Scope::Full {
            let mut old = self.old.borrow_mut();
            old.append(&mut self.young.borrow_mut());
            core::mem::swap(&mut *old, &mut self.young.borrow_mut());
        }
        let young = self.young.borrow();
        let has_owner =
            |handle: &&&WeakImpl| handle.state() == State::Live && handle.owner().is_some();
        self.undecided
            .borrow_mut()
            .extend(young.iter().filter(has_owner));
    }

    pub(super) fn visit(&self, visitor: &mut Visitor) {
        self.undecided.borrow_mut().retain(|handle| {
            let (State::Live, Some(owner)) = (handle.state(), handle.owner()) else {
                return false;
            };
            if visitor.is_address_marked(handle.target()) {
                return false;
            }
            let mut reason = AsciiLiteral::NULL;
            let wants_reason = visitor.analyzer().is_some();
            // SAFETY: the target has not been destroyed.
            let is_reachable = unsafe {
                (owner.is_reachable_from_opaque_roots)(
                    owner,
                    handle,
                    handle.context.get(),
                    visitor,
                    wants_reason.then_some(&mut reason),
                )
            };
            if is_reachable {
                // SAFETY: as above.
                unsafe { visitor.append_word(handle.target() as u64) };
                if let Some(analyzer) = visitor.analyzer() {
                    analyzer.opaque_root_reason(handle.target(), reason);
                }
            }
            !is_reachable
        });
    }

    pub(super) fn reap(&self, _: &Survivors) {
        self.undecided.borrow_mut().clear();
        let (mut old, mut free) = (self.old.borrow_mut(), self.free.borrow_mut());
        let mut dead = self.dead.borrow_mut();
        for handle in self.young.borrow_mut().drain(..) {
            match handle.state() {
                State::Deallocated => free.push(handle),
                State::Live if !container::of(handle.target()).is_marked(handle.target()) => {
                    handle.set_state(State::Dead);
                    dead.push(handle);
                }
                _ => old.push(handle),
            }
        }
    }

    pub(super) fn finalize_dead(&self) {
        loop {
            let Some(handle) = self.dead.borrow_mut().pop() else {
                return;
            };
            if handle.state() == State::Dead {
                handle.set_state(State::Finalized);
                if let Some(owner) = handle.owner()
                    // SAFETY: nothing has been swept since the target died.
                    && unsafe { (owner.finalize)(owner, handle, handle.context.get()) }
                {
                    handle.deallocate();
                }
            }
            if handle.state() == State::Deallocated {
                self.free.borrow_mut().push(handle);
            } else {
                self.old.borrow_mut().push(handle);
            }
        }
    }

    /// The heap is going away with everything in it.
    pub(super) fn finalize_all(&self) {
        loop {
            let mut handles = self.young.take();
            handles.append(&mut self.old.borrow_mut());
            handles.retain(|handle| handle.state() != State::Deallocated);
            let (live, rest): (Vec<_>, Vec<_>) = handles
                .into_iter()
                .partition(|handle| handle.state() == State::Live);
            self.old.borrow_mut().extend(rest);
            if live.is_empty() {
                return;
            }
            live.iter().for_each(|handle| handle.set_state(State::Dead));
            self.dead.borrow_mut().extend(live);
            self.finalize_dead();
        }
    }
}

impl Drop for WeakSet {
    /// A chunk stays for the handles that are still held.
    fn drop(&mut self) {
        for chunk in self.chunks.get_mut().drain(..) {
            // SAFETY: leaked by `grow`, and the lists are not read again.
            unsafe {
                let handles = &chunk.as_ref().0;
                if handles.iter().all(|it| it.state() == State::Deallocated) {
                    drop(Box::from_raw(chunk.as_ptr()));
                }
            }
        }
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
            handle: vm.heap.weak_set.allocate(cell.address(), owner, context),
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
