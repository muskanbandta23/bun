use core::ffi::{c_char, c_void};
use core::mem::ManuallyDrop;

use bun_engine_bytecode::Specialization;
use bun_engine_inspector::profiling::{
    AnyRealm, BlockRange, CodeRange, Coverage, Gaps, HeapProfiler, SnapshotKind, count_cells,
    count_protected_cells,
};
use bun_engine_macros::export;
use bun_engine_text::StringRef;
use bun_engine_vm::{
    CodeBlock, Executable, FastHashSet, FunctionExecutable, Gc, JsFunction, ScriptExecutable,
    SourceProvider, Value, Vm, options,
};

#[export]
fn coverage_enable(vm: &Vm) {
    Coverage::of(vm).enable(vm);
}

#[export]
fn coverage_is_enabled(vm: &Vm) -> bool {
    vm.collects_coverage().get()
}

#[export]
unsafe fn coverage_blocks(
    vm: &Vm,
    source_id: usize,
    with_gaps: bool,
    context: *mut c_void,
    take: unsafe extern "C" fn(*mut c_void, *const BlockRange, usize),
) {
    let gaps = if with_gaps { Gaps::With } else { Gaps::Without };
    let blocks = Coverage::of(vm).blocks(source_id, gaps);
    // SAFETY: the two go together.
    unsafe { take(context, blocks.as_ptr(), blocks.len()) }
}

#[export]
unsafe fn coverage_code_ranges(
    vm: &Vm,
    source_id: usize,
    context: *mut c_void,
    take: unsafe extern "C" fn(*mut c_void, *const CodeRange, usize),
) {
    let ranges = Coverage::of(vm).code_ranges(source_id);
    // SAFETY: as above.
    unsafe { take(context, ranges.as_ptr(), ranges.len()) }
}

#[export]
unsafe fn coverage_source_providers(
    vm: &Vm,
    context: *mut c_void,
    add: unsafe extern "C" fn(*mut c_void, &SourceProvider),
) {
    let mut seen = FastHashSet::default();
    vm.heap.for_each_live_cell(|cell| {
        if let Some(executable) = cell.downcast::<ScriptExecutable>()
            && seen.insert(executable.provider().id())
        {
            // SAFETY: as above.
            unsafe { add(context, executable.provider()) }
        }
    });
}

#[export]
unsafe fn heap_count_cells(
    vm: &Vm,
    only_protected: bool,
    context: *mut c_void,
    add: unsafe extern "C" fn(*mut c_void, *const c_char, usize),
) {
    let counts = if only_protected {
        count_protected_cells(vm)
    } else {
        count_cells(vm)
    };
    for (name, count) in counts.into_iter().filter(|it| !it.0.is_null()) {
        // SAFETY: as above.
        unsafe { add(context, name.as_str().as_ptr().cast(), count) }
    }
}

#[export]
fn heap_profiler_has_snapshot(vm: &Vm) -> bool {
    HeapProfiler::of(vm).has_snapshot()
}

#[export]
fn heap_profiler_clear_snapshots(vm: &Vm) {
    HeapProfiler::of(vm).clear_snapshots();
}

#[export]
unsafe fn heap_snapshot(vm: &Vm, is_for_debugging: bool) -> Option<StringRef> {
    let kind = if is_for_debugging {
        SnapshotKind::GcDebugging
    } else {
        SnapshotKind::Inspector
    };
    // SAFETY: C++ has the right to run scripts, and lends it for the call.
    let mut realms = unsafe { AnyRealm::assume(vm) };
    (HeapProfiler::of(vm).snapshot(&mut realms, kind, &|_| true)).ok()
}

#[export]
unsafe fn heap_v8_snapshot(vm: &Vm) -> Option<StringRef> {
    // SAFETY: as above.
    let mut realms = unsafe { AnyRealm::assume(vm) };
    HeapProfiler::of(vm).v8_snapshot(&mut realms).ok()
}

/// C++ frees them.
#[export]
unsafe fn heap_v8_snapshot_bytes(vm: &Vm, length: &mut usize) -> *mut u8 {
    // SAFETY: as above.
    let mut realms = unsafe { AnyRealm::assume(vm) };
    let Ok(bytes) = HeapProfiler::of(vm).v8_snapshot_bytes(&mut realms) else {
        *length = 0;
        return core::ptr::null_mut();
    };
    *length = bytes.len();
    ManuallyDrop::new(bytes).as_mut_ptr()
}

#[export]
fn executable_source_provider(executable: Gc<Executable>) -> Option<&'static SourceProvider> {
    let provider: *const SourceProvider = &**executable.downcast::<ScriptExecutable>()?.provider();
    // SAFETY: C++ does not use it for longer than the executable, which holds a reference.
    Some(unsafe { &*provider })
}

fn executable_of(function: Value) -> Option<Gc<FunctionExecutable>> {
    function.downcast::<JsFunction>()?.script_executable()
}

fn code_block_of(function: Value) -> Option<Gc<CodeBlock>> {
    let executable = executable_of(function)?;
    (executable.code_block(Specialization::Call))
        .or_else(|| executable.code_block(Specialization::Construct))
}

#[export]
fn function_set_never_inline(function: Value) {
    if let Some(executable) = executable_of(function) {
        executable.never_inline().set(true);
    }
}

#[export]
fn function_set_never_fully_optimize(function: Value) {
    if let Some(executable) = executable_of(function) {
        executable.never_fully_optimize().set(true);
    }
}

#[export]
fn function_optimize_next_invocation(function: Value) {
    if let Some(code_block) = code_block_of(function) {
        code_block.baseline().execution_counter().set(0);
    }
}

/// Very many if nothing optimizes, so that a loop that waits for it ends.
#[export]
fn function_optimization_count(function: Value) -> u32 {
    let optimizes = options().use_baseline_jit && options().use_quick_optimizer;
    match code_block_of(function) {
        Some(code_block) if optimizes => code_block.is_optimized().into(),
        Some(_) => 1_000_000,
        None => 0,
    }
}
