//! When to collect.

use core::cell::Cell;

use super::{BLOCK_SIZE, Heap, JsCell, MutatorState, Scope, barrier};
use crate::options;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HeapKind {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct HeapSizes {
    pub before_last_eden: usize,
    pub after_last_eden: usize,
    pub before_last_full: usize,
    pub after_last_full: usize,
    pub after_last: usize,
}

const OVERSIZED: usize = 64 * 1024;

pub(super) struct Policy {
    kind: HeapKind,
    max_heap_size: Cell<usize>,
    max_eden_size: Cell<usize>,
    allocated: Cell<usize>,
    last_oversized: Cell<usize>,
    extra_since_check: Cell<usize>,
    old_cell_bytes: Cell<usize>,
    extra_memory: Cell<usize>,
    untracked_extra_memory: Cell<usize>,
    array_buffer_bytes: Cell<usize>,
    should_collect_fully: Cell<bool>,
    is_safe_to_collect: Cell<bool>,
    sizes: Cell<HeapSizes>,
}

fn min_heap_size(kind: HeapKind) -> usize {
    let options = options();
    match kind {
        HeapKind::Large => options
            .large_heap_size
            .min((options.ram_size as f64 * options.small_heap_ram_fraction) as usize),
        HeapKind::Medium => options.medium_heap_size,
        HeapKind::Small => options.small_heap_size,
    }
}

fn proportional_heap_size(heap_size: usize) -> usize {
    const GROWTH_FUNCTION_THRESHOLD: usize = 16 << 30;
    let options = options();
    let (size, ram) = (heap_size as f64, options.ram_size as f64);
    let factor = if options.force_mini_vm_mode {
        options.mini_vm_heap_growth_factor
    } else if options.ram_size >= GROWTH_FUNCTION_THRESHOLD
        && !cfg!(all(target_os = "macos", target_arch = "x86_64"))
    {
        let steepness = options.heap_growth_steepness_factor;
        options.heap_growth_max_increase * (-steepness * size.min(ram) / ram).exp() + 1.0
    } else if size < ram * options.small_heap_ram_fraction {
        options.small_heap_growth_factor
    } else if size < ram * options.medium_heap_ram_fraction {
        options.medium_heap_growth_factor
    } else {
        options.large_heap_growth_factor
    };
    (factor * size) as usize
}

impl Policy {
    pub(super) fn new(kind: HeapKind) -> Policy {
        Policy {
            kind,
            max_heap_size: Cell::new(min_heap_size(kind)),
            max_eden_size: Cell::new(min_heap_size(kind)),
            allocated: Cell::new(0),
            last_oversized: Cell::new(0),
            extra_since_check: Cell::new(0),
            old_cell_bytes: Cell::new(0),
            extra_memory: Cell::new(0),
            untracked_extra_memory: Cell::new(0),
            array_buffer_bytes: Cell::new(0),
            should_collect_fully: Cell::new(false),
            is_safe_to_collect: Cell::new(false),
            sizes: Cell::default(),
        }
    }

    fn is_due(&self) -> bool {
        let allocated = self.allocated.get();
        let limit = options().gc_max_heap_size;
        if limit != 0 && allocated > limit {
            return true;
        }
        let allowed = self.max_heap_size.get() - self.sizes.get().after_last;
        // A large allocation that has just been made is alive, and says little about the rest.
        allocated > allowed
            && (self.kind != HeapKind::Large || self.last_oversized.get() <= allocated / 3)
    }

    pub(super) fn choose_scope(&self, requested: Option<Scope>) -> Scope {
        if !options().use_generational_gc || options().force_mini_vm_mode {
            return Scope::Full;
        }
        match requested {
            Some(scope) => scope,
            None if self.should_collect_fully.get() => Scope::Full,
            None => Scope::Eden,
        }
    }

    pub(super) fn will_collect(&self, scope: Scope) {
        let mut sizes = self.sizes.get();
        let before = sizes.after_last + self.allocated.get();
        match scope {
            Scope::Eden => sizes.before_last_eden = before,
            Scope::Full => {
                sizes.before_last_full = before;
                self.should_collect_fully.set(false);
                self.old_cell_bytes.set(0);
                self.extra_memory.set(0);
                self.untracked_extra_memory.set(0);
            }
        }
        self.sizes.set(sizes);
    }

    pub(super) fn did_collect(&self, scope: Scope, bytes_marked: usize, extra_memory: usize) {
        self.old_cell_bytes.update(|bytes| bytes + bytes_marked);
        self.extra_memory
            .update(|bytes| bytes.saturating_add(extra_memory));
        let size = self
            .old_cell_bytes
            .get()
            .saturating_add(self.extra_memory_size());
        let mut sizes = self.sizes.get();
        match scope {
            Scope::Full => {
                let max = min_heap_size(self.kind)
                    .max(proportional_heap_size(size))
                    .max(size.saturating_add(BLOCK_SIZE));
                self.max_heap_size.set(max);
                self.max_eden_size.set(max - size);
                sizes.after_last_full = size;
            }
            Scope::Eden => {
                let max = self.max_heap_size.get();
                let remaining = max.saturating_sub(size) as f64;
                if remaining / (max as f64) < options().min_eden_to_old_generation_ratio {
                    self.should_collect_fully.set(true);
                }
                self.max_heap_size
                    .set(max.max(size.saturating_add(self.max_eden_size.get())));
                sizes.after_last_eden = size;
            }
        }
        sizes.after_last = size;
        self.sizes.set(sizes);
        self.allocated.set(0);
        self.last_oversized.set(0);
    }

    fn extra_memory_size(&self) -> usize {
        self.extra_memory
            .get()
            .saturating_add(self.untracked_extra_memory.get())
            .saturating_add(self.array_buffer_bytes.get())
    }

    pub(super) fn max_eden_size(&self) -> usize {
        self.max_eden_size.get()
    }
}

impl Heap {
    pub(crate) fn notify_is_safe_to_collect(&self) {
        self.policy.is_safe_to_collect.set(true);
    }

    pub(super) fn can_collect(&self) -> bool {
        self.policy.is_safe_to_collect.get()
            && self.mutator_state() == MutatorState::Running
            && options().use_gc
    }

    #[inline]
    pub(crate) fn did_allocate(&self, bytes: usize) {
        self.policy
            .allocated
            .update(|allocated| allocated.saturating_add(bytes));
        if bytes >= OVERSIZED {
            self.policy.last_oversized.set(bytes);
        }
    }

    pub fn collect_if_necessary(&self) {
        if !self.policy.is_due() || !self.can_collect() {
            return;
        }
        if self.is_deferred() {
            self.did_defer_collection.set(true);
        } else {
            self.collect(None);
        }
    }

    #[cfg(debug_assertions)]
    pub(super) fn collect_if_stressed(&self) {
        let period = options().collect_every_n_allocations;
        if period == 0 || !self.can_collect() || self.is_deferred() {
            return;
        }
        let count = self.stress_countdown.get() + 1;
        self.stress_countdown.set(count);
        if count.is_multiple_of(period) {
            let is_full = count.is_multiple_of(period.saturating_mul(4));
            self.collect(Some(if is_full { Scope::Full } else { Scope::Eden }));
        }
    }

    /// `cell` has come to own that much more memory outside the heap. May collect.
    #[inline]
    pub fn report_extra_memory_allocated(&self, cell: &JsCell, bytes: usize) {
        const NEGLIGIBLE: usize = 256;
        if bytes > NEGLIGIBLE {
            self.note_extra_memory_allocated(cell, bytes);
            self.collect_if_extra_memory_adds_up();
        }
    }

    #[inline]
    pub(crate) fn collect_if_extra_memory_adds_up(&self) {
        if self.policy.extra_since_check.get() >= BLOCK_SIZE {
            self.policy.extra_since_check.set(0);
            self.collect_if_necessary();
        }
    }

    /// As `report_extra_memory_allocated`, but does not collect.
    pub(crate) fn note_extra_memory_allocated(&self, cell: &JsCell, bytes: usize) {
        self.did_allocate(bytes);
        self.policy
            .extra_since_check
            .update(|total| total.saturating_add(bytes));
        // An old cell is not visited for the first time again before the next full collection.
        if cell.state() == barrier::BLACK_THRESHOLD {
            self.policy
                .extra_memory
                .update(|total| total.saturating_add(bytes));
        }
    }

    pub(super) fn did_find_extra_memory(&self, bytes: usize) {
        self.policy
            .extra_memory
            .update(|total| total.saturating_add(bytes));
    }

    /// Memory that no visit will report. May collect.
    pub fn report_untracked_extra_memory(&self, bytes: usize) {
        self.policy
            .untracked_extra_memory
            .update(|total| total.saturating_add(bytes));
        self.did_allocate(bytes);
        self.collect_if_necessary();
    }

    /// So many of the bytes have already been reported as allocated.
    #[inline]
    pub(crate) fn did_add_array_buffer(&self, bytes: usize, already_reported: usize) {
        self.policy.array_buffer_bytes.update(|total| total + bytes);
        let new = bytes.saturating_sub(already_reported);
        self.did_allocate(new);
        self.policy
            .extra_since_check
            .update(|total| total.saturating_add(new));
    }

    pub(crate) fn did_remove_array_buffer(&self, bytes: usize) {
        self.policy
            .array_buffer_bytes
            .update(|total| total.saturating_sub(bytes));
    }

    pub fn set_initial_allocation_budget(&self, bytes: usize) {
        if self.last_collection_scope().is_none() && bytes > self.policy.max_eden_size.get() {
            self.policy.max_eden_size.set(bytes);
            self.policy.max_heap_size.set(bytes);
        }
    }

    pub fn sizes(&self) -> HeapSizes {
        self.policy.sizes.get()
    }

    pub fn bytes_allocated_this_cycle(&self) -> usize {
        self.policy.allocated.get()
    }

    pub fn array_buffer_size(&self) -> usize {
        self.policy.array_buffer_bytes.get()
    }

    pub fn extra_memory_size(&self) -> usize {
        self.policy.extra_memory_size()
    }

    pub fn block_bytes(&self) -> usize {
        self.blocks.borrow().len() * BLOCK_SIZE + self.precise_bytes.get()
    }

    pub fn capacity(&self) -> usize {
        self.block_bytes().saturating_add(self.extra_memory_size())
    }

    /// The count and the bytes of what the last collection marked.
    fn marked(&self) -> (usize, usize) {
        let (mut count, mut bytes) = (0, 0);
        self.for_each_directory(|directory| {
            for block in directory.blocks_with_marks() {
                count += block.marked_cell_count();
                bytes += block.marked_cell_count() * block.cell_size();
            }
        });
        for allocation in self.precise.borrow().iter().filter(|it| it.is_marked()) {
            count += 1;
            bytes += allocation.cell_size();
        }
        (count, bytes)
    }

    pub fn size(&self) -> usize {
        self.marked().1.saturating_add(self.extra_memory_size())
    }

    pub fn object_count(&self) -> usize {
        self.marked().0
    }
}
