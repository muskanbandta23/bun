use core::sync::atomic::{AtomicU64, Ordering};

use bun_engine_platform::clock;

use super::block::{Block, FIRST_VERSION, UNUSED_VERSION, next_version};
use super::conservative::{ConservativeRoots, Found};
use super::marking::{Marking, OpaqueRoots};
use super::verifier::Verifier;
use super::visitor::{Findings, REVISIT};
use super::{
    BLOCK_SIZE, DeferCollection, Gc, Heap, HeapAnalyzer, HeapGraphSink, JsCell, MutatorState,
    RootReason, Scope, Survivors, Visitor, container, root_alloc, stack,
};
use crate::options;

macro_rules! log {
    ($heap:expr, $($argument:tt)*) => {
        if $crate::options().log_gc {
            $heap.log(format_args!($($argument)*));
        }
    };
}

pub(super) use log;

impl Heap {
    #[cold]
    pub(super) fn log(&self, message: core::fmt::Arguments<'_>) {
        use std::io::Write;
        let line = format!("[GC<{self:p}>: {message}\n");
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    /// Does nothing where a collection cannot run.
    pub fn collect(&self, scope: Option<Scope>) {
        if !self.can_collect() {
            return;
        }
        if self.is_deferred() {
            return self.did_defer_collection.set(true);
        }
        self.collect_now(scope);
    }

    fn collect_now(&self, scope: Option<Scope>) {
        self.vm().clear_dead_stack();
        stack::with_registers_on_stack(|stack_top| self.run_collection(scope, stack_top));
        stack::clear_what_the_collector_left();
    }

    /// Also destroys everything that died.
    pub fn collect_and_sweep(&self, scope: Option<Scope>) {
        self.collect(scope);
        if self.mutator_state() == MutatorState::Running && !self.sweeps_synchronously() {
            self.sweep_synchronously();
        }
    }

    /// A full collection for a program that has had nothing to do for a while.
    pub fn collect_when_idle(&self) {
        self.is_idle_collection.set(true);
        self.collect(Some(Scope::Full));
        self.is_idle_collection.set(false);
    }

    /// A full collection that reports what it finds, which is there for as long as the result is. Reports nothing
    /// where a collection cannot run.
    pub fn collect_and_analyze(&self, sink: &mut dyn HeapGraphSink) -> DeferCollection<'_> {
        let can_collect = self.can_collect() && !self.is_deferred();
        let deferral = self.defer_collection();
        if can_collect {
            // SAFETY: a local.
            let mut analyzer = unsafe { HeapAnalyzer::new(sink) };
            self.analyzer.set(&raw mut analyzer);
            self.collect_now(Some(Scope::Full));
            self.sweep_synchronously();
        }
        deferral
    }

    fn sweeps_synchronously(&self) -> bool {
        options().sweep_synchronously || options().force_mini_vm_mode || self.policy.is_critical()
    }

    #[inline(never)]
    fn run_collection(&self, requested: Option<Scope>, stack_top: usize) {
        let start = clock::monotonic_seconds();
        let state = self.enter_state(MutatorState::Collecting);
        let scope = self.policy.choose_scope(requested);
        self.did_defer_collection.set(false);
        self.stop_allocating();
        let capacity = self.capacity();
        let has_nothing_to_find = scope == Scope::Eden && self.policy.allocated() == 0;
        self.collection_scope.set(Some(scope));
        self.policy.will_collect(scope);
        self.activity.will_collect(scope);
        self.notify_observers(|observer| observer.will_collect());
        (self.vm()).notify_collection_observers(|observer| observer.will_collect());

        let roots = if has_nothing_to_find {
            self.policy.did_collect(scope, 0, 0);
            None
        } else {
            Some(self.mark_and_clear_weak_references(scope, stack_top))
        };
        self.collection_scope.set(None);
        self.last_collection_scope.set(Some(scope));
        drop(state);

        if let Some(roots) = &roots {
            let state = self.enter_state(MutatorState::Sweeping);
            self.weak_set.finalize_dead();
            self.sweep_precise_allocations();
            drop(state);
            let _defer = self.defer_collection();
            self.vm().did_collect(scope, &roots.cells);
        }
        self.notify_observers(|observer| observer.did_collect(scope));
        (self.vm()).notify_collection_observers(|observer| observer.did_collect(scope));
        if roots.is_some() {
            self.give_memory_back(scope);
        }

        let seconds = clock::monotonic_seconds() - start;
        self.activity.did_collect(scope, seconds);
        if scope == Scope::Eden {
            self.schedule_full_collection(0);
        }
        let (before, after) = (capacity >> 10, self.sizes().after_last >> 10);
        let pause = seconds * 1e3;
        log!(
            self,
            "START M {before}kb => {scope:?}Collection, => {after}kb, p={pause:.3}ms END]"
        );
    }

    fn give_memory_back(&self, scope: Scope) {
        if scope == Scope::Full {
            self.strong_set.shrink();
        }
        if self.sweeps_synchronously() {
            self.sweep_synchronously();
        } else {
            if scope == Scope::Full {
                self.free_swept_empty_blocks(self.policy.remaining() / BLOCK_SIZE);
            }
            self.schedule_sweep(None);
        }
        if scope == Scope::Full && !cfg!(bun_asan) {
            bun_alloc::mimalloc::mi_collect(false);
        }
    }

    fn notify_observers(&self, notify: impl Fn(&super::HeapObserver)) {
        let _defer = self.defer_collection();
        let observers = self.observers.borrow().clone();
        for observer in observers {
            if self.observers.borrow().contains(&observer) {
                // SAFETY: valid until it is removed.
                notify(unsafe { &*observer });
            }
        }
    }

    /// Returns what was found conservatively.
    fn mark_and_clear_weak_references(&self, scope: Scope, stack_top: usize) -> Found {
        self.vm().will_collect(scope);
        self.begin_marking(scope);
        let roots = self.find_conservative_roots(stack_top);
        let marking = Marking::new(
            self.marking_version(),
            scope,
            &self.opaque_roots,
            self.analyzer.get(),
            options().number_of_gc_markers as usize,
        );
        let mut visitor = Visitor::new(&marking, self.spare_mark_stack.take());
        self.mark(&marking, &mut visitor, &roots);
        if options().verify_gc {
            self.verify_marking(&roots);
        }
        let findings = core::mem::take(&mut visitor.findings);
        let bytes_marked = findings.precise_bytes_marked + self.end_marking(scope);
        let mut stack = core::mem::take(&mut visitor.stack);
        const KEPT_ENTRIES: usize = 4096;
        stack.shrink_to(KEPT_ENTRIES);
        self.spare_mark_stack.set(stack);

        // SAFETY: it outlives the collection, and the visitor is done with it.
        if let Some(analyzer) = unsafe { self.analyzer.take().as_mut() } {
            self.for_each_live_cell(|cell| analyzer.analyze(cell));
        }

        let vm = self.vm();
        self.clear_weak_references(&Survivors { scope, vm }, &findings);
        self.policy
            .did_collect(scope, bytes_marked, findings.extra_memory);
        roots
    }

    fn begin_marking(&self, scope: Scope) {
        if scope == Scope::Full {
            let (old, new) = (self.marking_version(), next_version(self.marking_version()));
            self.for_each_directory(|directory| {
                directory.begin_full_marking();
                if new == FIRST_VERSION {
                    (directory.blocks().iter()).for_each(|block| block.forget_marks_unless_of(old));
                }
            });
            self.marking_version.store(new, Ordering::Relaxed);
            self.is_marking_full.store(true, Ordering::Relaxed);
            for allocation in self.precise.borrow().iter() {
                allocation.begin_full_marking();
            }
            self.first_young_precise.set(0);
            self.remembered.borrow_mut().clear();
            // SAFETY: nothing is marking.
            unsafe { self.opaque_roots.get_mut() }.clear();
        }
        self.weak_set.begin_marking(scope);
    }

    fn find_conservative_roots(&self, stack_top: usize) -> Found {
        let vm = self.vm();
        let (blocks, precise) = (self.blocks.borrow(), self.precise.borrow());
        // The old ones are marked as it is.
        let first = if options().verify_gc {
            0
        } else {
            self.first_young_precise.get()
        };
        let mut roots = ConservativeRoots::new(&blocks, &precise[first..], vm.code_address_range());
        // SAFETY: the stack of this thread, live allocations, and what the `Vm` vouches for.
        unsafe {
            roots.scan(stack_top, vm.stack_bounds().origin);
            root_alloc::for_each_range(|start, end| roots.scan(start, end));
            vm.for_each_conservative_range(|start, end| roots.scan(start, end));
        }
        roots.found
    }

    fn mark(&self, marking: &Marking, visitor: &mut Visitor, roots: &Found) {
        for &cell in &roots.cells {
            visitor.append_hidden_cell(cell);
        }
        for &base in &roots.auxiliary {
            // SAFETY: live.
            unsafe { visitor.mark_auxiliary(base) };
        }
        visitor.replace_root_reason(RootReason::ExecutingCode);
        (self.vm()).visit_code_on_stack(&roots.code_addresses, visitor);
        visitor.replace_root_reason(RootReason::ProtectedValues);
        for &cell in self.protected.borrow().keys() {
            // SAFETY: kept alive so far.
            unsafe { visitor.append_word(cell as u64) };
        }
        visitor.replace_root_reason(RootReason::StrongHandles);
        self.strong_set.visit(visitor);
        self.vm().visit_roots(visitor);
        self.execute_constraints(visitor, false);
        self.mark_to_fixpoint(marking, visitor);
    }

    fn execute_constraints(&self, visitor: &mut Visitor, depend_on_marking: bool) {
        visitor.replace_root_reason(RootReason::None);
        for constraint in self.constraints.borrow().iter() {
            if constraint.depends_on_marking == depend_on_marking {
                constraint.execute(visitor);
            }
        }
    }

    fn drain(&self, marking: &Marking, visitor: &mut Visitor) {
        loop {
            visitor.stack.append(&mut self.remembered.borrow_mut());
            let raced = core::mem::take(&mut *marking.raced.lock());
            (visitor.stack).extend(raced.into_iter().map(|cell| cell | REVISIT));
            if visitor.stack.is_empty() {
                break;
            }
            marking.drain(visitor);
        }
        for block in visitor.findings.prepared_blocks.drain(..) {
            // SAFETY: something in it is marked.
            let block = unsafe { Block::containing(block) };
            block.directory().did_prepare_for_marking(block);
        }
    }

    fn mark_to_fixpoint(&self, marking: &Marking, visitor: &mut Visitor) {
        /// A chain of entries of weak maps would take a round for each link.
        const ROUNDS_BEFORE_REMEMBERING_EPHEMERONS: usize = 8;
        self.drain(marking, visitor);
        for space in self.spaces_with_output_constraints.borrow().iter() {
            let directory = space.allocator.directory();
            for block in directory.into_iter().flat_map(|it| it.blocks_with_marks()) {
                visitor.push_output_of_block(block);
            }
        }
        let weak_handles = self.weak_set.handles_with_owner(marking.scope);
        for round in 0.. {
            if round == ROUNDS_BEFORE_REMEMBERING_EPHEMERONS {
                visitor.start_remembering_ephemerons();
            }
            let visits = visitor.findings.visit_count;
            visitor.push_weak_handles(&weak_handles);
            let cells = core::mem::take(&mut visitor.findings.output_constraints);
            let tables = core::mem::take(&mut visitor.findings.ephemeron_tables);
            visitor.push_output_of_cells(&cells);
            visitor.push_output_of_cells(&tables);
            visitor.findings.output_constraints = cells;
            if !visitor.remembers_ephemerons() {
                visitor.findings.ephemeron_tables = tables;
            }
            visitor.replace_root_reason(RootReason::Output);
            self.vm().visit_output_constraints(visitor);
            self.execute_constraints(visitor, true);
            self.drain(marking, visitor);
            if visitor.findings.visit_count == visits {
                return;
            }
        }
    }

    #[cold]
    fn verify_marking(&self, roots: &Found) {
        let (opaque_roots, no_analyzer) = (OpaqueRoots::default(), core::ptr::null_mut());
        let marking = Marking::new(UNUSED_VERSION, Scope::Full, &opaque_roots, no_analyzer, 1);
        let mut visitor = Visitor::verifying(&marking, Verifier::new(self));
        self.mark(&marking, &mut visitor, roots);
    }

    /// Returns the bytes of the cells in blocks that this collection has marked.
    fn end_marking(&self, scope: Scope) -> usize {
        self.is_marking_full.store(false, Ordering::Relaxed);
        let version = next_version(self.allocation_version());
        let mut bytes_marked = 0;
        self.for_each_directory(|directory| {
            if version == FIRST_VERSION {
                (directory.blocks().iter()).for_each(|block| block.forget_allocation_versions());
            }
            bytes_marked += directory.end_marking(scope);
        });
        self.allocation_version.store(version, Ordering::Relaxed);
        for allocation in &self.precise.borrow()[self.first_young_precise.get()..] {
            allocation.end_marking();
        }
        self.steal_cursor.set(0);
        self.reset_sweeper();
        bytes_marked
    }

    fn clear_weak_references(&self, survivors: &Survivors, findings: &Findings) {
        self.weak_set.reap(survivors);
        for &address in &findings.weak_words {
            // SAFETY: inside a cell that has survived.
            let word =
                unsafe { AtomicU64::from_ptr(core::ptr::with_exposed_provenance_mut(address)) };
            let cell = word.load(Ordering::Relaxed) as usize;
            if cell != 0 && !container::of(cell).is_marked(cell) {
                word.store(0, Ordering::Relaxed);
            }
        }
        for &cell in &findings.to_reconcile {
            // SAFETY: visited by this collection, and the method of its class.
            unsafe {
                let cell = Gc::<JsCell>::from_address(cell);
                (cell.class().ops().reconcile_weak_references)(&cell, survivors);
            }
        }
        self.vm().reconcile_weak_references(survivors);
    }

    fn sweep_precise_allocations(&self) {
        let first = self.first_young_precise.get();
        let mut precise = self.precise.borrow_mut();
        let dead: Vec<_> = (precise.extract_if(first.., |it| !it.is_marked())).collect();
        for (index, allocation) in precise.iter().enumerate().skip(first) {
            allocation.index.set(index as u32);
        }
        self.first_young_precise.set(precise.len());
        drop(precise);
        let _state = self.enter_destruction();
        for allocation in dead {
            self.precise_bytes
                .update(|bytes| bytes - allocation.cell_size());
            // SAFETY: dead, and out of the list.
            unsafe {
                allocation.destroy_cell();
                allocation.free();
            }
        }
    }
}
