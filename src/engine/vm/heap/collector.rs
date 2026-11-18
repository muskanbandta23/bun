use core::sync::atomic::{AtomicU64, Ordering};

use super::block::{FIRST_VERSION, next_version};
use super::conservative::ConservativeRoots;
use super::marking::Marking;
use super::visitor::{Findings, REVISIT};
use super::{
    BLOCK_SIZE, Gc, Heap, HeapAnalyzer, JsCell, MutatorState, RootReason, Scope, Survivors,
    Visitor, container, root_alloc, stack,
};
use crate::options;

impl Heap {
    /// Does nothing where a collection cannot run.
    pub fn collect(&self, scope: Option<Scope>) {
        if !self.can_collect() {
            return;
        }
        if self.is_deferred() {
            return self.did_defer_collection.set(true);
        }
        self.vm().clear_dead_stack();
        stack::with_registers_on_stack(|stack_top| self.run_collection(scope, stack_top));
        self.vm().clear_dead_stack();
    }

    /// Also destroys everything that died.
    pub fn collect_and_sweep(&self, scope: Option<Scope>) {
        self.collect(scope);
        if self.mutator_state() == MutatorState::Running {
            self.sweep_synchronously();
        }
    }

    /// A full collection that reports what it finds.
    pub fn collect_and_analyze(&self, analyzer: &mut HeapAnalyzer) {
        self.analyzer.set(analyzer);
        self.collect(Some(Scope::Full));
        self.analyzer.set(core::ptr::null_mut());
    }

    #[inline(never)]
    fn run_collection(&self, requested: Option<Scope>, stack_top: usize) {
        let vm = self.vm();
        let state = self.enter_state(MutatorState::Collecting);
        let scope = self.policy.choose_scope(requested);
        self.did_defer_collection.set(false);
        self.stop_allocating();
        self.collection_scope.set(Some(scope));
        self.policy.will_collect(scope);
        self.notify_observers(|observer| observer.will_collect());
        vm.will_collect(scope);

        self.begin_marking(scope);
        let marking = Marking::new(self, scope, options().number_of_gc_markers as usize);
        let mut visitor = Visitor::new(&marking, self.spare_mark_stack.take());
        let cells_on_stack = self.mark_conservative_roots(scope, stack_top, &mut visitor);
        self.mark_roots(&mut visitor);
        self.mark_to_fixpoint(&marking, &mut visitor);
        self.end_marking(scope);
        let findings = core::mem::take(&mut visitor.findings);
        self.spare_mark_stack
            .set(core::mem::take(&mut visitor.stack));

        let survivors = Survivors { scope, vm };
        self.clear_weak_references(&survivors, &findings);
        self.policy
            .did_collect(scope, findings.bytes_marked, findings.extra_memory);
        self.collection_scope.set(None);
        self.last_collection_scope.set(Some(scope));
        drop(state);

        {
            let _state = self.enter_state(MutatorState::Sweeping);
            self.weak_set.finalize_dead();
            self.sweep_precise_allocations(scope);
            vm.did_collect(scope, &cells_on_stack);
        }
        self.notify_observers(|observer| observer.did_collect(scope));
        if options().sweep_synchronously || options().force_mini_vm_mode {
            self.sweep_synchronously();
        } else if scope == Scope::Full {
            self.strong_set.shrink();
            self.free_empty_blocks(self.policy.max_eden_size() / BLOCK_SIZE);
        }
    }

    fn notify_observers(&self, notify: impl Fn(&super::HeapObserver)) {
        let observers = self.observers.borrow().clone();
        for observer in observers {
            if self.observers.borrow().contains(&observer) {
                // SAFETY: valid until it is removed.
                notify(unsafe { &*observer });
            }
        }
    }

    fn begin_marking(&self, scope: Scope) {
        if scope == Scope::Full {
            let version = next_version(self.marking_version());
            assert!(version != FIRST_VERSION);
            self.marking_version.store(version, Ordering::Relaxed);
            self.is_marking_full.store(true, Ordering::Relaxed);
            self.for_each_directory(|directory| directory.begin_full_marking());
            for allocation in self.precise.borrow().iter() {
                allocation.begin_full_marking();
            }
            self.first_young_precise.set(0);
            self.remembered.borrow_mut().clear();
            self.opaque_roots.lock().clear();
        }
        self.weak_set.begin_marking(scope);
    }

    /// Returns the cells that were found.
    fn mark_conservative_roots(
        &self,
        _: Scope,
        stack_top: usize,
        visitor: &mut Visitor,
    ) -> Vec<usize> {
        let vm = self.vm();
        let blocks = self.blocks.borrow();
        let mut precise = self.precise.borrow_mut();
        let young = &mut precise[self.first_young_precise.get()..];
        young.sort_unstable_by_key(|allocation| allocation.cell());
        let mut roots = ConservativeRoots::new(&blocks, young, vm.code_address_range());
        // SAFETY: the stack of this thread, live allocations, and what the `Vm` vouches for.
        unsafe {
            roots.scan(stack_top, vm.stack_bounds().origin);
            root_alloc::for_each_range(|start, end| roots.scan(start, end));
            vm.for_each_conservative_range(|start, end| roots.scan(start, end));
        }
        for &cell in &roots.cells {
            visitor.append_hidden_cell(cell);
        }
        for &base in &roots.auxiliary {
            // SAFETY: live.
            unsafe { visitor.mark_auxiliary(base) };
        }
        vm.visit_code_on_stack(&roots.code_addresses, visitor);
        roots.cells
    }

    fn mark_roots(&self, visitor: &mut Visitor) {
        set_root_reason(visitor, RootReason::StrongHandles);
        self.strong_set.visit(visitor);
        set_root_reason(visitor, RootReason::ProtectedValues);
        for &cell in self.protected.borrow().keys() {
            // SAFETY: kept alive so far.
            unsafe { visitor.append_word(cell as u64) };
        }
        set_root_reason(visitor, RootReason::StrongReferences);
        self.vm().visit_roots(visitor);
        set_root_reason(visitor, RootReason::None);
    }

    fn mark_to_fixpoint(&self, marking: &Marking, visitor: &mut Visitor) {
        let mut is_first_round = true;
        loop {
            visitor.stack.append(&mut self.remembered.borrow_mut());
            let raced = core::mem::take(&mut *marking.raced.lock());
            visitor
                .stack
                .extend(raced.into_iter().map(|cell| cell | REVISIT));
            marking.drain(visitor);

            let visits = visitor.findings.visit_count;
            set_root_reason(visitor, RootReason::WeakSets);
            self.weak_set.visit(visitor);
            set_root_reason(visitor, RootReason::Output);
            for index in 0..visitor.findings.output_constraints.len() {
                let cell = visitor.findings.output_constraints[index];
                // SAFETY: visited by this collection.
                let cell = unsafe { Gc::<JsCell>::from_address(cell) };
                visitor.with_referrer(&cell, |visitor| {
                    // SAFETY: the method of its class.
                    unsafe { (cell.class().ops().visit_output_constraints)(&cell, visitor) }
                });
            }
            self.vm().visit_output_constraints(visitor);
            set_root_reason(visitor, RootReason::Embedder);
            if is_first_round {
                for space in self.spaces_with_output_constraints.borrow().iter() {
                    self.for_each_marked_cell_in(space, |cell| {
                        visitor.with_referrer(&cell, |visitor| {
                            // SAFETY: the method of its class.
                            unsafe { (cell.class().ops().visit_output_constraints)(&cell, visitor) }
                        });
                    });
                }
            }
            for constraint in self.constraints.borrow().iter() {
                if is_first_round || constraint.depends_on_marking {
                    constraint.execute(visitor);
                }
            }
            set_root_reason(visitor, RootReason::None);
            is_first_round = false;

            if visitor.stack.is_empty()
                && visitor.findings.visit_count == visits
                && self.remembered.borrow().is_empty()
                && marking.raced.lock().is_empty()
            {
                return;
            }
        }
    }

    fn end_marking(&self, scope: Scope) {
        self.is_marking_full.store(false, Ordering::Relaxed);
        let version = next_version(self.allocation_version());
        if version == FIRST_VERSION {
            self.for_each_directory(|directory| {
                for block in directory.blocks() {
                    block.forget_allocation_versions();
                }
            });
        }
        self.allocation_version.store(version, Ordering::Relaxed);
        self.for_each_directory(|directory| directory.end_marking(scope));
        for allocation in &self.precise.borrow()[self.first_young_precise.get()..] {
            allocation.end_marking();
        }
        self.steal_cursor.set(0);
        self.sweep_cursor.set(0);
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

    fn sweep_precise_allocations(&self, _: Scope) {
        let first = self.first_young_precise.get();
        let dead: Vec<_> = self
            .precise
            .borrow_mut()
            .extract_if(first.., |allocation| !allocation.is_marked())
            .collect();
        for (index, allocation) in self.precise.borrow().iter().enumerate().skip(first) {
            allocation.index.set(index as u32);
        }
        self.first_young_precise.set(self.precise.borrow().len());
        for allocation in dead {
            self.precise_bytes
                .update(|bytes| bytes - allocation.cell_size());
            // SAFETY: dead, and out of the list.
            unsafe { allocation.destroy() };
        }
    }
}

fn set_root_reason(visitor: &mut Visitor, reason: RootReason) {
    if let Some(analyzer) = visitor.analyzer() {
        analyzer.root_reason = reason;
    }
}
