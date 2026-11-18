//! What a program has run, where it spends its time and what it keeps in memory.

mod any_realm;
mod coverage;
mod heap;
mod json;

use bun_engine_vm::Vm;
use bun_engine_vm::heap::Survivors;

pub use any_realm::AnyRealm;
pub use coverage::{BlockRange, CodeRange, CodeRangeKind, Coverage, Gaps, cover};
pub use heap::{HeapProfiler, SnapshotKind, class_name, count_cells, count_protected_cells};

/// For `Hooks::reconcile_weak_references`.
pub fn reconcile_weak_references(vm: &Vm, survivors: &Survivors) {
    if let Some(profiler) = vm.existing_extension::<HeapProfiler>() {
        profiler.forget_dead_cells(survivors);
    }
}
