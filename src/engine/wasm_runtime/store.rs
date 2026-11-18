//! What the run time keeps for a thread and its heap.

use core::cell::{Cell, RefCell};
use core::ptr;
use core::sync::atomic::{AtomicU8, Ordering};
use std::rc::Rc;
use std::sync::Arc;

use crate::code::{Compilers, RetiredCode};
use crate::engine::Engine;
use crate::host::{Host, Urgency, Vm};
use crate::memory::Backoff;
use crate::registry::{Registration, Registry};
use crate::value::HostCell;

/// What has to be left of the stack below the limit, for stubs and operations.
pub const STACK_RESERVE: usize = if cfg!(debug_assertions) {
    128 << 10
} else {
    32 << 10
};

pub struct Store {
    engine: &'static Engine,
    vm: Vm,
    stack_limit: Cell<usize>,
    stack_limits: Registry<usize>,
    pub(crate) compilers: RefCell<Compilers>,
    pub(crate) retired: Arc<RetiredCode>,
    pub(crate) collections_for_speed: Backoff,
}

impl Store {
    /// # Safety
    /// `vm` is that of this thread, and outlives all uses but drops of what is made with the store. `stack_limit` is
    /// as for [`Store::set_stack_limit`].
    pub unsafe fn new(engine: &'static Engine, vm: Vm, stack_limit: usize) -> Rc<Self> {
        Rc::new(Self {
            engine,
            vm,
            stack_limit: Cell::new(stack_limit),
            stack_limits: Registry::default(),
            compilers: RefCell::default(),
            retired: Arc::default(),
            collections_for_speed: Backoff::default(),
        })
    }

    #[inline]
    pub fn engine(&self) -> &'static Engine {
        self.engine
    }

    #[inline]
    pub fn vm(&self) -> Vm {
        self.vm
    }

    #[inline]
    pub(crate) fn host(&self) -> (&'static dyn Host, Vm) {
        (self.engine.host(), self.vm)
    }

    /// # Safety
    /// The stack of the thread extends for [`STACK_RESERVE`] bytes below `limit`, and can be touched in any order.
    pub unsafe fn set_stack_limit(&self, limit: usize) {
        self.stack_limit.set(limit);
        self.stack_limits.write_all(|| limit);
    }

    pub(crate) fn stack_limit(&self) -> usize {
        self.stack_limit.get()
    }

    /// Whether there is this much left for native code.
    pub(crate) fn has_stack_for(&self, bytes: usize) -> bool {
        let here = 0u8;
        let end = self.stack_limit().saturating_sub(STACK_RESERVE);
        (&raw const here).addr().saturating_sub(end) >= bytes
    }

    /// # Safety
    /// `word` is valid for writes until it is removed.
    pub(crate) unsafe fn add_stack_limit(&self, word: *mut usize) -> Registration {
        // SAFETY: the caller's.
        unsafe {
            word.write(self.stack_limit());
            self.stack_limits.add(word)
        }
    }

    pub(crate) fn remove_stack_limit(&self, registration: Registration) {
        self.stack_limits.remove(registration);
    }

    /// After a cell has been stored in `cell` or in what it traces.
    #[inline]
    pub(crate) fn write_barrier(&self, cell: HostCell) {
        let state = cell.field(self.engine.layout().cell_state);
        // SAFETY: every cell has this byte.
        let state = unsafe { AtomicU8::from_ptr(state) }.load(Ordering::Relaxed);
        if state <= self.engine.barrier_threshold() {
            self.engine.host().write_barrier(self.vm, cell);
        }
    }

    pub(crate) fn collect_garbage(&self, urgency: Urgency) {
        self.engine.host().collect_garbage(self.vm, urgency);
    }

    pub(crate) fn set_top_frame(&self, fp: usize) {
        let place = self.vm.0 + self.engine.layout().vm_top_frame as usize;
        // SAFETY: the VM has the word there.
        unsafe { ptr::with_exposed_provenance_mut::<usize>(place).write(fp) };
    }

    /// For the collector, on any thread. Frees code that has been replaced.
    ///
    /// # Safety
    /// The thread of the store is in native code, and all return addresses on its stacks are among `words`.
    pub unsafe fn release_code_not_on_stack(&self, words: &[usize]) {
        // SAFETY: the caller's.
        unsafe { self.retired.release_all_but(words) };
    }
}
