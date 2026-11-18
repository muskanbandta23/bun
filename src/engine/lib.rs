//! Assembles the parts of the engine.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use bun_engine_vm::heap::HeapKind;
use bun_engine_vm::{CallFrame, CodeBlock, Cx, Hooks, RealmSlot, Tier, Vm, VmRef, call};

fn initialize_realm_slot(cx: &Cx, slot: RealmSlot) {
    let _ = bun_engine_vm::initialize_module_slot(cx, slot)
        || bun_engine_builtins::initialize_slot(cx, slot)
        || bun_engine_intl::initialize_slot(cx, slot)
        || bun_engine_temporal::initialize_slot(cx, slot)
        || bun_engine_wasm::initialize_slot(cx, slot);
}

fn tier_up(_: &Vm, code_block: &CodeBlock, _: &CallFrame, _: Option<u32>) -> usize {
    if code_block.tier() == Tier::Interpreter {
        return bun_engine_baseline::compile_and_enter(code_block);
    }
    code_block.execution_counter().set(i32::MIN);
    bun_engine_baseline::resume_entry()
}

fn hooks() -> Hooks {
    Hooks {
        interpreter_entry: bun_engine_interpreter::entry(),
        catch_address: bun_engine_baseline::catch_address,
        native_call_entry: |_, _| call::native_call_entry(),
        jettison: |_, _| {},
        cover: bun_engine_inspector::profiling::cover,
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
        parse_json: |cx, text| {
            let text = cx.new_string(text)?.into();
            cx.invoke(
                cx.json_object().into(),
                bun_engine_text::names::parse,
                &[text],
            )
        },
        make_wasm_module: |cx, _, _, _, _| Err(cx.throw_type_error("Not supported: WebAssembly")),
        find_wasm_handler: |_, _| None,
        wasm_frame: |_, _, _, _| unreachable!(),
        wasm_names: |_, _| unreachable!(),
        visit_roots: |_, _| {},
        visit_code_on_stack: |_, _, _| {},
        reconcile_weak_references: bun_engine_inspector::profiling::reconcile_weak_references,
        did_collect: |_, _| {},
    }
}

/// For the current thread. `bun_engine_vm::set_options` has been called.
pub fn create_vm(heap_kind: HeapKind) -> VmRef {
    static HOOKS: OnceLock<Hooks> = OnceLock::new();
    VmRef::new(HOOKS.get_or_init(hooks), heap_kind)
}
