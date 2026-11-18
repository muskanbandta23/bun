//! Assembles the parts of the engine.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use bun_engine_vm::heap::HeapKind;
use bun_engine_vm::{Cx, Hooks, RealmSlot, VmRef, call};

fn initialize_realm_slot(cx: &Cx, slot: RealmSlot) {
    let _ = bun_engine_vm::initialize_module_slot(cx, slot)
        || bun_engine_builtins::initialize_slot(cx, slot)
        || bun_engine_intl::initialize_slot(cx, slot);
}

fn hooks() -> Hooks {
    Hooks {
        interpreter_entry: bun_engine_interpreter::entry(),
        catch_address: |_, _| bun_engine_interpreter::catch_entry(),
        native_call_entry: |_, _| call::native_call_entry(),
        jettison: |_, _| {},
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
        temporal_to_locale_string: |cx, this, _, _| this.as_value().to_string(cx).map(Into::into),
        find_wasm_handler: |_, _| None,
        debugger_did_parse: |_, _, _| {},
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
