//! Linear memories.

mod access;
mod budget;
mod pages;
mod system;

use core::cell::{Cell, RefCell};
use core::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use bun_engine_asm::Width;
use bun_engine_wasm_compiler::abi::{
    GUARD_SIZE, LOW_GUARD_SIZE, MemoryMode, MemoryView, PAGE_SIZE_LOG2,
};
use bun_engine_wasm_format::limits::{MAX_MEMORY32_PAGES, MAX_MEMORY64_PAGES};
use bun_engine_wasm_format::{Memory as MemoryType, OutOfMemory};

pub(crate) use self::access::{copy_bytes, fill_bytes};
use self::budget::{ADDRESS_SPACE, COMMITTED, Need};
pub use self::pages::Pages;
use crate::host::Urgency;
use crate::registry::{Registration, Registry};
use crate::store::Store;
use crate::value::HostCell;

/// That of the longest buffer.
const MAX_LENGTH: u64 = 1 << 32;
const MAX_PAGES: u64 = MAX_LENGTH >> PAGE_SIZE_LOG2;
const GUARDED_SIZE: u64 = (1 << 32) + GUARD_SIZE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrowError {
    /// More than a memory with such addresses can declare.
    InvalidSize,
    ExceedsMaximum,
    OutOfMemory,
}

/// What all linear memories of the process take, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MemoryUsage {
    pub reserved: u64,
    pub committed: u64,
}

pub fn memory_usage() -> MemoryUsage {
    MemoryUsage {
        reserved: ADDRESS_SPACE.used(),
        committed: COMMITTED.used(),
    }
}

/// A shared memory on its way to another thread.
#[derive(Clone)]
pub struct SharedMemory {
    ty: MemoryType,
    mode: MemoryMode,
    pages: Arc<Pages>,
}

impl SharedMemory {
    pub fn pages(&self) -> &Arc<Pages> {
        &self.pages
    }
}

pub struct Memory {
    store: Rc<Store>,
    ty: MemoryType,
    mode: MemoryMode,
    pages: RefCell<Arc<Pages>>,
    /// Where a fault means that an access is out of bounds.
    faulting: Range<usize>,
    views: Registry<MemoryView>,
    owner: Cell<Option<HostCell>>,
}

impl Memory {
    /// The limits are valid for the type of address, and a shared memory has a maximum. `wanted` is a wish.
    pub fn new(
        store: &Rc<Store>,
        ty: MemoryType,
        wanted: MemoryMode,
    ) -> Result<Rc<Self>, OutOfMemory> {
        if ty.pages.initial > MAX_PAGES {
            return Err(OutOfMemory);
        }
        let length = ty.pages.initial << PAGE_SIZE_LOG2;
        let room = room_for(&ty);
        let is_worth_guarding = wanted == MemoryMode::Guarded
            && ty.address == Width::W32
            && room != 0
            && store.engine().options().guard_memories;
        let guarded = is_worth_guarding.then(|| reserve_guarded(store)).flatten();
        let (mode, pages) = match guarded {
            Some(pages) => (MemoryMode::Guarded, Some(pages)),
            None if ty.shared => {
                let pages = insisting(store, || Pages::reserve(room, Need::Function));
                (MemoryMode::Checked, pages)
            }
            None => {
                let pages = Pages::reserve(room, Need::Speed)
                    .or_else(|| insisting(store, || Pages::reserve(length, Need::Bounded)));
                (MemoryMode::Checked, pages)
            }
        };
        let pages = pages.ok_or(OutOfMemory)?;
        insisting(store, || {
            pages.lock_growth().extend_to(length).then_some(())
        })
        .ok_or(OutOfMemory)?;
        if ADDRESS_SPACE.is_short(Need::Speed) || COMMITTED.is_short(Need::Function) {
            store.collect_garbage(Urgency::Soon);
        }
        Ok(Self::with_pages(store, ty, mode, Arc::new(pages)))
    }

    pub fn from_shared(store: &Rc<Store>, shared: SharedMemory) -> Rc<Self> {
        Self::with_pages(store, shared.ty, shared.mode, shared.pages)
    }

    fn with_pages(
        store: &Rc<Store>,
        ty: MemoryType,
        mode: MemoryMode,
        pages: Arc<Pages>,
    ) -> Rc<Self> {
        let stays = mode == MemoryMode::Guarded || ty.shared;
        Rc::new(Self {
            store: Rc::clone(store),
            ty,
            mode,
            faulting: if stays { pages.reservation() } else { 0..0 },
            pages: RefCell::new(pages),
            views: Registry::default(),
            owner: Cell::new(None),
        })
    }

    /// `None` unless it is shared.
    pub fn share(&self) -> Option<SharedMemory> {
        self.ty.shared.then(|| SharedMemory {
            ty: self.ty,
            mode: self.mode,
            pages: self.pages(),
        })
    }

    /// As it was declared.
    pub fn ty(&self) -> &MemoryType {
        &self.ty
    }

    pub fn mode(&self) -> MemoryMode {
        self.mode
    }

    /// A memory that is neither guarded nor shared may leave them for others when it grows.
    pub fn pages(&self) -> Arc<Pages> {
        Arc::clone(&self.pages.borrow())
    }

    /// In bytes.
    #[inline]
    pub fn len(&self) -> u64 {
        self.pages.borrow().len()
    }

    pub fn page_count(&self) -> u64 {
        self.len() >> PAGE_SIZE_LOG2
    }

    pub fn owner(&self) -> Option<HostCell> {
        self.owner.get()
    }

    /// The cell that instances keep alive along with the memory.
    pub fn set_owner(&self, owner: HostCell) {
        self.owner.set(Some(owner));
    }

    /// Gives the number of pages before.
    pub fn grow(&self, pages: u64) -> Result<u64, GrowError> {
        let old = match self.try_grow(pages) {
            Err(Denied::ForNow) => {
                self.store.collect_garbage(Urgency::Now);
                self.try_grow(pages)
            }
            outcome => outcome,
        };
        let old = old.map_err(|denied| match denied {
            Denied::ForNow => GrowError::OutOfMemory,
            Denied::ForGood(error) => error,
        })?;
        if self.ty.shared && pages == 0 {
            return Ok(old);
        }
        self.views.write_all(|| self.view());
        let (host, vm) = self.store.host();
        host.memory_grew(vm, self, old << PAGE_SIZE_LOG2);
        if pages != 0 && COMMITTED.is_short(Need::Function) {
            self.store.collect_garbage(Urgency::Soon);
        }
        Ok(old)
    }

    fn try_grow(&self, added: u64) -> Result<u64, Denied> {
        let pages = self.pages();
        let growth = pages.lock_growth();
        let old = pages.len() >> PAGE_SIZE_LOG2;
        let declarable = match self.ty.address {
            Width::W32 => MAX_MEMORY32_PAGES,
            Width::W64 => MAX_MEMORY64_PAGES,
        };
        let new = old.checked_add(added).filter(|&new| new <= declarable);
        let new = new.ok_or(Denied::ForGood(GrowError::InvalidSize))?;
        if new > MAX_PAGES {
            return Err(Denied::ForGood(GrowError::OutOfMemory));
        }
        if self.ty.pages.maximum.is_some_and(|maximum| new > maximum) {
            return Err(Denied::ForGood(GrowError::ExceedsMaximum));
        }
        let length = new << PAGE_SIZE_LOG2;
        let grew = if length <= pages.reserved() {
            growth.extend_to(length)
        } else {
            self.move_from(&pages, length)
        };
        grew.then_some(old).ok_or(Denied::ForNow)
    }

    fn move_from(&self, old: &Pages, length: u64) -> bool {
        assert!(self.faulting.is_empty());
        let room = room_for(&self.ty);
        let new = Pages::reserve(room, Need::Speed)
            .or_else(|| Pages::reserve((length * 2).min(room), Need::Bounded))
            .or_else(|| Pages::reserve(length, Need::Bounded))
            .filter(|new| new.lock_growth().extend_to(length));
        let Some(new) = new else { return false };
        // SAFETY: both have at least this many accessible bytes.
        unsafe { copy_bytes(new.base(), old.base(), old.len() as usize) };
        self.pages.replace(Arc::new(new));
        true
    }

    fn view(&self) -> MemoryView {
        let pages = self.pages.borrow();
        let (length, shared_length) = match self.ty.shared {
            true => (0, pages.length_address()),
            false => (pages.len(), 0),
        };
        MemoryView {
            base: pages.base().addr(),
            length,
            shared_length,
        }
    }

    /// # Safety
    /// `view` is valid for writes until it is removed.
    pub(crate) unsafe fn add_view(&self, view: *mut MemoryView) -> Registration {
        // SAFETY: the caller's.
        unsafe {
            view.write(self.view());
            self.views.add(view)
        }
    }

    pub(crate) fn remove_view(&self, registration: Registration) {
        self.views.remove(registration);
    }

    /// For the handler of faults.
    pub(crate) fn faults_at(&self, address: usize) -> bool {
        self.faulting.contains(&address)
    }

    pub(crate) fn store(&self) -> &Rc<Store> {
        &self.store
    }
}

enum Denied {
    /// Collecting garbage may help.
    ForNow,
    ForGood(GrowError),
}

/// The most bytes it can ever have.
fn room_for(ty: &MemoryType) -> u64 {
    ty.pages.maximum.unwrap_or(MAX_PAGES).min(MAX_PAGES) << PAGE_SIZE_LOG2
}

fn insisting<T>(store: &Store, attempt: impl Fn() -> Option<T>) -> Option<T> {
    attempt().or_else(|| {
        store.collect_garbage(Urgency::Now);
        attempt()
    })
}

fn reserve_guarded(store: &Store) -> Option<Pages> {
    let attempt = || Pages::reserve_with_margin(LOW_GUARD_SIZE, GUARDED_SIZE, Need::Speed);
    attempt().or_else(|| {
        if ADDRESS_SPACE.used() < GUARDED_SIZE {
            return None;
        }
        store.collections_for_speed.attempt(|| {
            store.collect_garbage(Urgency::Now);
            attempt()
        })
    })
}

/// Tries ever more rarely what keeps failing.
#[derive(Default)]
pub(crate) struct Backoff {
    failures: Cell<u32>,
    skips_left: Cell<u32>,
}

impl Backoff {
    fn attempt<T>(&self, attempt: impl FnOnce() -> Option<T>) -> Option<T> {
        if let Some(left) = self.skips_left.get().checked_sub(1) {
            self.skips_left.set(left);
            return None;
        }
        let outcome = attempt();
        let failures = match outcome {
            Some(_) => 0,
            None => (self.failures.get() + 1).min(16),
        };
        self.failures.set(failures);
        self.skips_left.set((1 << failures) - 1);
        outcome
    }
}
