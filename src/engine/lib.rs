//! Assembles the parts of the engine.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use bun_engine_vm::heap::HeapKind;
use bun_engine_vm::{Cx, Hooks, RealmSlot, VmRef, call};

fn initialize_realm_slot(cx: &Cx, slot: RealmSlot) {
    let _ = bun_engine_vm::initialize_module_slot(cx, slot)
        || bun_engine_builtins::initialize_slot(cx, slot)
        || bun_engine_intl::initialize_slot(cx, slot)
        || bun_engine_temporal::initialize_slot(cx, slot);
}

fn hooks() -> Hooks {
    Hooks {
        interpreter_entry: bun_engine_interpreter::entry(),
        catch_address: |_, _| bun_engine_interpreter::catch_entry(),
        native_call_entry: |_, _| call::native_call_entry(),
        jettison: |_, _| {},
        debug: |_, _, _, _| Ok(()),
        debug_await: |_, _, _, _| Ok(()),
        cover: bun_engine_inspector::profiling::cover,
        access_handler: |_, _, _| 0,
        tier_up: |_, code_block, _, _| {
            code_block.execution_counter().set(i32::MIN);
            0
        },
        initialize_realm: bun_engine_builtins::initialize_realm,
        initialize_realm_slot,
        realm_watchpoint_conditions: bun_engine_builtins::watchpoint_conditions,
        did_start_having_a_bad_time: |_, _| {},
        did_declare_global_lexical: |_, _, _| {},
        builtin_source: bun_engine_builtins::builtin_source,
        grow_wasm_memory: |_, _, _| false,
        temporal_to_locale_string: bun_engine_intl::temporal_to_locale_string,
        parse_json: |cx, text| {
            let text = cx.new_string(text)?.into();
            cx.invoke(cx.json_object().into(), bun_engine_text::names::parse, &[text])
        },
        make_wasm_module: |cx, _, _, _, _| Err(cx.throw_type_error("Not supported: WebAssembly")),
        find_wasm_handler: |_, _| None,
        wasm_frame: |_, _, _, _| unreachable!(),
        wasm_names: |_, _| unreachable!(),
        debugger_did_parse: |_, _, _| {},
        debugger_microtask_event: |_, _, _, _| {},
        debugger_will_unwind: |_, _| {},
        debugger_did_unwind_frame: |_, _| {},
        visit_roots: |_, _| {},
        visit_code_on_stack: |_, _, _| {},
        reconcile_weak_references: |_, _| {},
        did_collect: |_, _| {},
    }
}

/// For the current thread. `bun_engine_vm::set_options` has been called.
pub fn create_vm(heap_kind: HeapKind) -> VmRef {
    static HOOKS: OnceLock<Hooks> = OnceLock::new();
    VmRef::new(HOOKS.get_or_init(hooks), heap_kind)
}
