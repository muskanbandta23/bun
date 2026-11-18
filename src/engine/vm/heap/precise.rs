use core::cell::Cell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use bun_alloc::default_alloc;
use bun_engine_macros::FieldOffsets;

use super::block::ATOM_SIZE;
use super::{CellAttributes, Heap};

/// The address of a cell that has an allocation to itself has this bit set. That of a cell in a block has not.
pub const PRECISE_BIT: usize = ATOM_SIZE / 2;

/// The header of a cell that is too large for a block.
#[derive(FieldOffsets)]
#[repr(C)]
pub struct PreciseAllocation {
    heap: *const Heap,
    cell_size: usize,
    pub(super) attributes: CellAttributes,
    is_marked: AtomicBool,
    is_newly_allocated: Cell<bool>,
    /// In the list of the heap, while no collection runs.
    pub(super) index: Cell<u32>,
}

const HEADER_SIZE: usize =
    (size_of::<PreciseAllocation>() - PRECISE_BIT).next_multiple_of(ATOM_SIZE) + PRECISE_BIT;

fn size_with_header(cell_size: usize) -> Option<usize> {
    cell_size
        .checked_add(HEADER_SIZE)?
        .checked_next_multiple_of(ATOM_SIZE)
}

impl PreciseAllocation {
    pub const HEADER_SIZE: usize = HEADER_SIZE;

    pub(super) fn try_create(
        heap: &Heap,
        cell_size: usize,
        attributes: CellAttributes,
        allocate: fn(usize) -> *mut c_void,
    ) -> Option<&'static PreciseAllocation> {
        let memory = allocate(size_with_header(cell_size)?).cast::<PreciseAllocation>();
        if memory.is_null() {
            return None;
        }
        assert!(memory.expose_provenance().is_multiple_of(ATOM_SIZE));
        // SAFETY: a new allocation with room for the header and the first word of the cell.
        unsafe {
            memory.write(PreciseAllocation {
                heap,
                cell_size,
                attributes,
                is_marked: AtomicBool::new(false),
                is_newly_allocated: Cell::new(true),
                index: Cell::new(0),
            });
            memory.byte_add(HEADER_SIZE).cast::<u64>().write(0);
            Some(&*memory)
        }
    }

    /// # Safety
    /// `cell` is as for `of_cell`. If this succeeds, nothing uses the old address.
    pub(super) unsafe fn try_resize(
        cell: usize,
        cell_size: usize,
    ) -> Option<&'static PreciseAllocation> {
        let old = core::ptr::with_exposed_provenance_mut(cell - HEADER_SIZE);
        // SAFETY: from `try_create`, and the caller's.
        unsafe {
            let memory = default_alloc::realloc(old, size_with_header(cell_size)?);
            let allocation = memory.cast::<PreciseAllocation>().as_mut()?;
            assert!(memory.expose_provenance().is_multiple_of(ATOM_SIZE));
            allocation.cell_size = cell_size;
            Some(allocation)
        }
    }

    /// # Safety
    /// `cell` is the start of a cell with `PRECISE_BIT` set.
    #[inline(always)]
    pub(super) unsafe fn of_cell(cell: usize) -> &'static PreciseAllocation {
        // SAFETY: the caller's.
        unsafe { &*core::ptr::with_exposed_provenance(cell - HEADER_SIZE) }
    }

    #[inline(always)]
    pub(super) fn cell(&self) -> usize {
        core::ptr::from_ref(self).addr() + HEADER_SIZE
    }

    #[inline(always)]
    pub(super) fn cell_size(&self) -> usize {
        self.cell_size
    }

    #[inline(always)]
    pub(super) fn heap(&self) -> &Heap {
        // SAFETY: outlives the allocation.
        unsafe { &*self.heap }
    }

    /// A pointer to the elements of a storage may be one word past the end.
    pub(super) fn contains(&self, address: usize) -> bool {
        address.wrapping_sub(self.cell()) <= self.cell_size + size_of::<u64>()
    }

    #[inline(always)]
    pub(super) fn is_marked(&self) -> bool {
        self.is_marked.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub(super) fn test_and_set_marked(&self) -> bool {
        self.is_marked() || self.is_marked.swap(true, Ordering::Relaxed)
    }

    pub(super) fn is_live(&self) -> bool {
        self.is_marked() || self.is_newly_allocated.get()
    }

    pub(super) fn begin_full_marking(&self) {
        self.is_newly_allocated.set(self.is_live());
        self.is_marked.store(false, Ordering::Relaxed);
    }

    pub(super) fn end_marking(&self) {
        self.is_newly_allocated.set(false);
    }

    /// # Safety
    /// The cell is dead, and this is the first time.
    pub(super) unsafe fn destroy_cell(&self) {
        let header = core::ptr::with_exposed_provenance_mut::<u32>(self.cell());
        // SAFETY: a header that is not zero belongs to an initialized cell.
        unsafe {
            if let Some(destroy) = self.attributes.destroy
                && *header != 0
            {
                destroy(header.cast());
                *header = 0;
            }
        }
    }

    /// # Safety
    /// The cell has been destroyed, and nothing uses the allocation afterwards.
    pub(super) unsafe fn free(&self) {
        let memory = core::ptr::with_exposed_provenance_mut(self.cell() - HEADER_SIZE);
        // SAFETY: the caller's.
        unsafe { default_alloc::free(memory) };
    }
}
