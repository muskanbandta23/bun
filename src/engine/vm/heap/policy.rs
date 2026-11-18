//! When to collect.

use core::cell::Cell;

use bun_engine_platform::{clock, machine};

use super::collector::log;
use super::{BLOCK_SIZE, CellState, DeferCollection, Heap, JsCell, MutatorState, Scope};
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
    /// The process has taken most of the memory that it can get.
    is_critical: Cell<bool>,
    checks_since_measurement: Cell<u32>,
    last_collection_end: Cell<f64>,
    stress_count: Cell<u32>,
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

fn proportional_heap_size(heap_size: usize, footprint: Option<usize>) -> usize {
    const GROWTH_FUNCTION_THRESHOLD: usize = 16 << 30;
    let options = options();
    let (size, ram) = (heap_size as f64, options.ram_size as f64);
    let footprint = footprint.unwrap_or(heap_size) as f64;
    let factor = if options.force_mini_vm_mode {
        options.mini_vm_heap_growth_factor
    } else if options.ram_size >= GROWTH_FUNCTION_THRESHOLD
        && !cfg!(all(target_os = "macos", target_arch = "x86_64"))
    {
        let steepness = options.heap_growth_steepness_factor;
        options.heap_growth_max_increase * (-steepness * size.min(ram) / ram).exp() + 1.0
    } else if footprint < ram * options.small_heap_ram_fraction {
        options.small_heap_growth_factor
    } else if footprint < ram * options.medium_heap_ram_fraction {
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
            is_critical: Cell::new(false),
            checks_since_measurement: Cell::new(0),
            last_collection_end: Cell::new(clock::monotonic_seconds()),
            stress_count: Cell::new(0),
        }
    }

    fn measure_footprint(&self) -> Option<usize> {
        let footprint = machine::memory_footprint();
        let threshold = machine::ram_size() as f64 * options().critical_gc_memory_threshold;
        self.is_critical
            .set(footprint.is_some_and(|it| it as f64 > threshold));
        self.checks_since_measurement.set(0);
        footprint
    }

    pub(super) fn is_critical(&self) -> bool {
        self.is_critical.get()
    }

    /// In this cycle.
    fn allowed(&self) -> usize {
        let allowed = self.max_heap_size.get() - self.sizes.get().after_last;
        if !self.is_critical() {
            return allowed;
        }
        let above_threshold = 1.0 - options().critical_gc_memory_threshold;
        allowed.min((options().ram_size as f64 * above_threshold) as usize / 4)
    }

    pub(super) fn remaining(&self) -> usize {
        self.allowed().saturating_sub(self.allocated())
    }

    pub(super) fn allocated(&self) -> usize {
        self.allocated.get()
    }

    fn is_due(&self) -> bool {
        const CHECKS_BETWEEN_MEASUREMENTS: u32 = 100;
        const CONTINUOUS_PERIOD: f64 = 0.001;
        self.checks_since_measurement.update(|checks| checks + 1);
        if self.checks_since_measurement.get() >= CHECKS_BETWEEN_MEASUREMENTS {
            self.measure_footprint();
        }
        let (allocated, limit) = (self.allocated(), options().gc_max_heap_size);
        if (limit != 0 && allocated > limit)
            || (options().collect_continuously
                && clock::monotonic_seconds() - self.last_collection_end.get() > CONTINUOUS_PERIOD)
        {
            return true;
        }
        // A large allocation that has just been made is alive, and says little about the rest.
        allocated > self.allowed()
            && (self.is_critical()
                || self.kind != HeapKind::Large
                || self.last_oversized.get() <= allocated / 3)
    }

    /// Whether a collection would become due before so much more is allocated, and has something to find now.
    fn is_due_ahead_of(&self, bytes: usize) -> bool {
        let (allocated, allowed) = (self.allocated.get(), self.allowed());
        allocated.saturating_add(bytes) > allowed
            && allocated.saturating_sub(self.last_oversized.get()) >= allowed / 8
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
                    .max(proportional_heap_size(size, self.measure_footprint()))
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
        self.last_collection_end.set(clock::monotonic_seconds());
    }

    pub(super) fn did_grow_old_cell(&self, bytes: usize) {
        self.old_cell_bytes.update(|total| total + bytes);
    }

    fn extra_memory_size(&self) -> usize {
        self.extra_memory
            .get()
            .saturating_add(self.untracked_extra_memory.get())
            .saturating_add(self.array_buffer_bytes.get())
    }
}

impl Heap {
    pub(crate) fn notify_is_safe_to_collect(&self) {
        self.policy.is_safe_to_collect.set(true);
        log!(self, "starting ]");
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
        if !self.can_collect() {
            return;
        }
        if self.policy.is_due() {
            self.collect(None);
        } else {
            self.schedule_eden_collection();
        }
    }

    /// For making about so many bytes of cells that are all alive in the end, which a collection in the middle of it
    /// could only waste time on. May collect now.
    #[inline]
    pub(crate) fn defer_collection_for(&self, bytes: usize) -> DeferCollection<'_> {
        #[cfg(debug_assertions)]
        self.collect_if_stressed();
        if (self.did_defer_collection.get() || bytes >= BLOCK_SIZE)
            && (self.policy.is_due() || self.policy.is_due_ahead_of(bytes))
        {
            self.collect(None);
        }
        self.defer_collection()
    }

    pub(super) fn will_allocate_slowly(&self) {
        assert!(
            !self.is_destroying_cells.get() && self.mutator_state() != MutatorState::Collecting,
            "an allocation while the collector is at work"
        );
        #[cfg(not(debug_assertions))]
        self.collect_if_stressed();
        self.collect_if_necessary();
    }

    pub(super) fn collect_if_stressed(&self) {
        let period = options().collect_every_n_allocations;
        if period == 0 || !self.can_collect() || self.is_deferred() {
            return;
        }
        let count = self.policy.stress_count.get().wrapping_add(1);
        self.policy.stress_count.set(count);
        if count.is_multiple_of(period) {
            let is_full = count.is_multiple_of(period.saturating_mul(4))
                && !options().use_only_eden_collections_under_stress;
            self.collect_and_sweep(Some(if is_full { Scope::Full } else { Scope::Eden }));
        }
    }

    /// Whether it is worth trying again.
    #[cold]
    pub(super) fn collect_for_lack_of_memory(&self) -> bool {
        if !self.can_collect() || self.is_deferred() {
            return false;
        }
        self.collect_and_sweep(Some(Scope::Full));
        true
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
        if cell.state() != CellState::DefinitelyWhite as u8 {
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
            self.activity.suspend_until_next_collection();
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
}
