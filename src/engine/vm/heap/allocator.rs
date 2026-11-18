use core::cell::{Cell, OnceCell};
use core::mem::offset_of;

use super::block::{Block, FreeInterval};
use super::block_memory::BlockSource;
use super::directory::Directory;
use super::{CellAttributes, Heap};

/// Hands out cells of one size and kind. Generated code bumps `start` itself and calls `allocate_slow` at `end`.
#[repr(C)]
pub struct Allocator {
    start: Cell<usize>,
    end: Cell<usize>,
    next_interval: Cell<usize>,
    secret: Cell<u64>,
    cell_size: u32,
    free_list_size: Cell<u32>,
    current_block: Cell<Option<&'static Block>>,
    pub(super) attributes: CellAttributes,
    pub(super) source: BlockSource,
    directory: OnceCell<Box<Directory>>,
}

// SAFETY: a part of the collector.
unsafe impl super::GcFree for Allocator {}

impl Allocator {
    pub const OFFSET_OF_START: usize = offset_of!(Allocator, start);
    pub const OFFSET_OF_END: usize = offset_of!(Allocator, end);
    pub const OFFSET_OF_NEXT_INTERVAL: usize = offset_of!(Allocator, next_interval);
    pub const OFFSET_OF_SECRET: usize = offset_of!(Allocator, secret);
    pub const OFFSET_OF_CELL_SIZE: usize = offset_of!(Allocator, cell_size);

    pub(super) const fn new(
        cell_size: usize,
        attributes: CellAttributes,
        source: BlockSource,
    ) -> Allocator {
        Allocator {
            start: Cell::new(0),
            end: Cell::new(0),
            next_interval: Cell::new(0),
            secret: Cell::new(0),
            cell_size: cell_size as u32,
            free_list_size: Cell::new(0),
            current_block: Cell::new(None),
            attributes,
            source,
            directory: OnceCell::new(),
        }
    }

    #[inline(always)]
    pub fn cell_size(&self) -> usize {
        self.cell_size as usize
    }

    pub(super) fn directory(&self) -> Option<&Directory> {
        self.directory.get().map(|directory| &**directory)
    }

    /// The address of a new cell, or zero if there is no memory. May collect.
    #[inline(always)]
    pub(crate) fn try_allocate(&self, heap: &Heap) -> usize {
        self.try_allocate_sized(heap, self.cell_size())
    }

    /// # Safety
    /// The cell is initialized, as what the allocator is for, before the next allocation.
    pub unsafe fn try_allocate_uninitialized(&self, heap: &Heap) -> usize {
        self.try_allocate(heap)
    }

    /// `cell_size` is that of the allocator, which the caller may know as a constant.
    #[inline(always)]
    pub(crate) fn try_allocate_sized(&self, heap: &Heap, cell_size: usize) -> usize {
        debug_assert!(cell_size == self.cell_size());
        #[cfg(debug_assertions)]
        heap.collect_if_stressed();
        let cell = self.start.get();
        if cell < self.end.get() {
            self.start.set(cell + cell_size);
            return cell;
        }
        self.allocate_slow(heap)
    }

    /// The address of a new cell, if that takes no call. The caller initializes it before the next allocation.
    #[inline(always)]
    pub fn try_bump(&self) -> Option<usize> {
        #[cfg(debug_assertions)]
        if crate::options().collect_every_n_allocations != 0 {
            return None;
        }
        let cell = self.start.get();
        (cell < self.end.get()).then(|| {
            self.start.set(cell + self.cell_size());
            cell
        })
    }

    #[inline(never)]
    pub fn allocate_slow(&self, heap: &Heap) -> usize {
        if self.next_interval.get() == 0 && !self.refill(heap) {
            return 0;
        }
        if self.start.get() == self.end.get() {
            let interval = self.next_interval.get();
            // SAFETY: the chain that the last sweep of the current block wrote.
            let (length, next) = unsafe { FreeInterval::read(interval, self.secret.get()) };
            self.start.set(interval);
            self.end.set(interval + length);
            self.next_interval.set(next);
        }
        let cell = self.start.get();
        self.start.set(cell + self.cell_size());
        cell
    }

    #[cold]
    #[inline(never)]
    fn refill(&self, heap: &Heap) -> bool {
        let directory = self.directory.get_or_init(|| {
            heap.register_allocator(self);
            Box::new(Directory::new(heap, self))
        });
        if let Some(block) = self.current_block.take() {
            block.did_consume_free_list();
            heap.did_allocate(self.free_list_size.take() as usize);
        }
        heap.collect_if_necessary();
        // What ran at the end of a collection may have allocated.
        if self.current_block.get().is_some() {
            return self.start.get() != self.end.get()
                || self.next_interval.get() != 0
                || self.refill(heap);
        }
        let secret = heap.next_secret();
        let Some((block, swept)) = directory.find_free_cells(secret) else {
            return false;
        };
        self.current_block.set(Some(block));
        self.secret.set(secret);
        self.free_list_size.set(swept.free_bytes as u32);
        self.next_interval.set(swept.first_interval);
        true
    }

    /// Gives the free list up, so that the block knows which of its cells are live.
    pub(super) fn stop_allocating(&self, heap: &Heap) {
        let Some(block) = self.current_block.take() else {
            return;
        };
        let (start, end) = (self.start.take(), self.end.take());
        let (mut next, secret) = (self.next_interval.take(), self.secret.get());
        let size = self.free_list_size.take() as usize;
        if start == end && next == 0 {
            heap.did_allocate(size);
            return block.did_consume_free_list();
        }
        let position = if start == end { next } else { start };
        let mut unused = end - start;
        let rest = core::iter::from_fn(|| {
            let interval = core::mem::take(&mut next);
            (interval != 0).then(|| {
                // SAFETY: as in `allocate_slow`.
                let (length, following) = unsafe { FreeInterval::read(interval, secret) };
                next = following;
                unused += length;
                (interval, interval + length)
            })
        });
        let current = (start != end).then_some((start, end));
        block.stop_allocating(position, current.into_iter().chain(rest));
        heap.did_allocate(size - unused);
        block.directory().did_stop_allocating(block);
    }

    /// Whether `cell`, which is in the block being allocated from, has not been handed out.
    pub(super) fn is_free(&self, cell: usize) -> bool {
        if (self.start.get()..self.end.get()).contains(&cell) {
            return true;
        }
        let mut interval = self.next_interval.get();
        while interval != 0 && interval <= cell {
            // SAFETY: as in `allocate_slow`.
            let (length, next) = unsafe { FreeInterval::read(interval, self.secret.get()) };
            if cell < interval + length {
                return true;
            }
            interval = next;
        }
        false
    }
}
