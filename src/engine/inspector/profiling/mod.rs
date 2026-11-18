//! What a program has run, where it spends its time and what it keeps in memory.

mod any_realm;
mod coverage;
mod heap;
mod json;
mod sampling;

use bun_engine_vm::heap::Survivors;
use bun_engine_vm::{Visitor, Vm};

pub use any_realm::AnyRealm;
pub use coverage::{BlockRange, CodeRange, CodeRangeKind, Coverage, Gaps, cover};
pub use heap::{HeapProfiler, SnapshotKind, class_name, count_cells, count_protected_cells};
pub use sampling::{Profile, SampledFrame, SampledFrameKind, SamplingProfiler, Trace};

/// For `Hooks::visit_roots`.
pub fn visit_roots(vm: &Vm, visitor: &mut Visitor) {
    if let Some(profiler) = vm.existing_extension::<SamplingProfiler>() {
        profiler.visit(vm, visitor);
    }
}

/// For `Hooks::will_destroy`.
pub fn will_destroy(vm: &Vm) {
    if let Some(profiler) = vm.existing_extension::<SamplingProfiler>() {
        profiler.shut_down();
    }
}

/// For `Hooks::reconcile_weak_references`.
pub fn reconcile_weak_references(vm: &Vm, survivors: &Survivors) {
    if let Some(profiler) = vm.existing_extension::<HeapProfiler>() {
        profiler.forget_dead_cells(survivors);
    }
}
