//! Where blocks come from. Those of shapes are in one range, so that 32 bits tell where a shape is.

use core::sync::atomic::{AtomicUsize, Ordering};

use bun_threading::Guarded;

use super::block::BLOCK_SIZE;
use super::virtual_memory;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BlockSource {
    General,
    Shapes,
}

#[unsafe(export_name = "engine_shape_base")]
static SHAPE_BASE: AtomicUsize = AtomicUsize::new(0);
static SHAPE_ID_LIMIT: AtomicUsize = AtomicUsize::new(0);

struct ShapeRange {
    size: usize,
    committed: usize,
    next: usize,
    free: Vec<usize>,
}

static SHAPE_RANGE: Guarded<ShapeRange> = Guarded::new(ShapeRange {
    size: 0,
    committed: 0,
    next: 0,
    free: Vec::new(),
});

/// The address that the id of a shape is an offset from.
#[inline(always)]
pub fn shape_base() -> usize {
    // SAFETY: written once, before the thread that reads it has a heap.
    unsafe { *SHAPE_BASE.as_ptr() }
}

/// Above the id of every shape.
pub(super) fn shape_id_limit() -> usize {
    SHAPE_ID_LIMIT.load(Ordering::Relaxed)
}

pub(super) fn initialize() {
    const LARGEST: usize = 1 << 32;
    const SMALLEST: usize = 1 << 24;
    let mut range = SHAPE_RANGE.lock();
    if range.size != 0 {
        return;
    }
    let mut size = LARGEST;
    let base = loop {
        match virtual_memory::reserve(size) {
            Some(address) => break address & !(BLOCK_SIZE - 1),
            None if size > SMALLEST => size /= 2,
            None => bun_alloc::out_of_memory(),
        }
    };
    SHAPE_BASE.store(base, Ordering::Relaxed);
    // The first block may start before the reservation, and no shape has the id zero.
    (range.size, range.committed, range.next) = (size, BLOCK_SIZE, BLOCK_SIZE);
}

impl BlockSource {
    pub(super) fn allocate(self) -> Option<usize> {
        match self {
            BlockSource::General => general::allocate(),
            BlockSource::Shapes => allocate_for_shapes(),
        }
    }

    /// # Safety
    /// The block came from `allocate` of the same source, and nothing uses it any more.
    pub(super) unsafe fn free(self, block: usize) {
        match self {
            // SAFETY: the caller's.
            BlockSource::General => unsafe { general::free(block) },
            BlockSource::Shapes => {
                if BLOCK_SIZE.is_multiple_of(bun_alloc::page_size()) {
                    // SAFETY: whole pages, and the caller's.
                    unsafe { virtual_memory::discard(block, BLOCK_SIZE) };
                }
                SHAPE_RANGE.lock().free.push(block);
            }
        }
    }
}

fn allocate_for_shapes() -> Option<usize> {
    const COMMIT_SIZE: usize = 1 << 20;
    let mut range = SHAPE_RANGE.lock();
    if let Some(block) = range.free.pop() {
        // SAFETY: discarded by `free`.
        unsafe { virtual_memory::reuse(block, BLOCK_SIZE) };
        return Some(block);
    }
    if range.next == range.committed {
        let size = COMMIT_SIZE.min(range.size - range.committed);
        let start = (shape_base() + range.committed).next_multiple_of(bun_alloc::page_size());
        let end = shape_base() + range.committed + size;
        // SAFETY: inside the reservation, past what is in use.
        if size == 0 || !unsafe { virtual_memory::commit(start, end - start) } {
            return None;
        }
        range.next = (start - shape_base()).next_multiple_of(BLOCK_SIZE);
        range.committed += size;
    }
    let block = shape_base() + range.next;
    range.next += BLOCK_SIZE;
    SHAPE_ID_LIMIT.store(range.next, Ordering::Relaxed);
    Some(block)
}

mod general {
    use core::alloc::Layout;

    use super::BLOCK_SIZE;

    const LAYOUT: Layout = match Layout::from_size_align(BLOCK_SIZE, BLOCK_SIZE) {
        Ok(layout) => layout,
        Err(_) => unreachable!(),
    };

    pub(super) fn allocate() -> Option<usize> {
        // SAFETY: the size is not zero.
        let memory = unsafe { std::alloc::alloc(LAYOUT) };
        (!memory.is_null()).then(|| memory.expose_provenance())
    }

    pub(super) unsafe fn free(block: usize) {
        // SAFETY: the caller's.
        unsafe { std::alloc::dealloc(core::ptr::with_exposed_provenance_mut(block), LAYOUT) }
    }
}
