//! Assembles the parts of the engine.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use bun_engine_inspector::profiling;
use bun_engine_vm::heap::HeapKind;
use bun_engine_vm::{CallFrame, CodeBlock, Cx, Hooks, RealmSlot, Tier, Vm, VmRef, call};

fn initialize_realm_slot(cx: &Cx, slot: RealmSlot) {
    let _ = bun_engine_vm::initialize_module_slot(cx, slot)
        || bun_engine_builtins::initialize_slot(cx, slot)
        || bun_engine_intl::initialize_slot(cx, slot)
        || bun_engine_temporal::initialize_slot(cx, slot)
        || bun_engine_wasm::initialize_slot(cx, slot);
}

fn tier_up(vm: &Vm, code_block: &CodeBlock, frame: &CallFrame, loop_offset: Option<u32>) -> usize {
    if code_block.tier() == Tier::Interpreter && bun_engine_baseline::install(code_block) {
        return bun_engine_baseline::resume_entry(code_block);
    }
    match bun_engine_optimizer::tier_up(vm, code_block, frame, loop_offset) {
        0 => bun_engine_baseline::resume_entry(code_block),
        address => address,
    }
}

fn hooks() -> Hooks {
    Hooks {
        interpreter_entry: bun_engine_interpreter::entry(),
        catch_address: bun_engine_baseline::catch_address,
        native_call_entry: |_, _| call::native_call_entry(),
        jettison: bun_engine_optimizer::jettison,
        cover: profiling::cover,
        access_handler: bun_engine_baseline::access_handler,
        tier_up,
        initialize_realm: bun_engine_builtins::initialize_realm,
        initialize_realm_slot,
        realm_watchpoint_conditions: bun_engine_builtins::watchpoint_conditions,
        did_start_having_a_bad_time: |_, _| {},
        did_declare_global_lexical: |_, _, _| {},
        builtin_source: bun_engine_builtins::builtin_source,
        grow_wasm_memory: |_, _, _| false,
        temporal_to_locale_string: bun_engine_intl::temporal_to_locale_string,
        make_wasm_module: |cx, _, _, _, _| Err(cx.throw_type_error("Not supported: WebAssembly")),
        find_wasm_handler: |_, _| None,
        wasm_frame: |_, _, _, _| unreachable!(),
        sampled_wasm_frame: |_, _, _, _| None,
        wasm_names: |_, _| unreachable!(),
        visit_roots: profiling::visit_roots,
        visit_code_on_stack: |_, _, _| {},
        reconcile_weak_references: profiling::reconcile_weak_references,
        did_collect: |_, _| {},
        will_destroy: profiling::will_destroy,
    }
}

/// For the current thread. `bun_engine_vm::set_options` has been called.
pub fn create_vm(heap_kind: HeapKind) -> VmRef {
    static HOOKS: OnceLock<Hooks> = OnceLock::new();
    VmRef::new(HOOKS.get_or_init(hooks), heap_kind)
}
