use core::ops::Range;

use bun_engine_bytecode::{FunctionKind, Visibility};
use bun_engine_text::PropertyName;

use crate::heap::{Scope, Survivors};
use crate::{
    BuiltinId, CallFrame, CodeBlock, CompileError, Cx, GlobalObject, Intrinsic, PropertyCondition,
    RealmSlot, RealmWatchpoint, SourceProvider, Visitor, Vm,
};

/// What this crate needs from those above it.
pub struct Hooks {
    /// The entry of an executable whose code is interpreted.
    pub interpreter_entry: usize,
    /// Where code that is not optimized goes on at the handler at that offset.
    pub catch_address: fn(&CodeBlock, u32) -> usize,
    pub native_call_entry: fn(&Vm, Intrinsic) -> usize,
    pub jettison: fn(&Vm, &CodeBlock),
    /// Sets the eager slots but `global_this`.
    pub initialize_realm: fn(&Cx),
    pub initialize_realm_slot: fn(&Cx, RealmSlot),
    /// `None` if it does not hold any more.
    pub realm_watchpoint_conditions: fn(&Cx, RealmWatchpoint) -> Option<Vec<PropertyCondition>>,
    pub did_start_having_a_bad_time: fn(&Vm, &GlobalObject),
    pub did_declare_global_lexical: fn(&Vm, &GlobalObject, PropertyName<'_>),
    /// The sources of all built-ins, one after the other.
    pub builtins_text: &'static str,
    pub builtin_source: fn(BuiltinId) -> BuiltinSource,
    /// The built-ins that others refer to as `@name`.
    pub private_builtins: fn() -> &'static [(PropertyName<'static>, BuiltinId)],
    pub find_wasm_handler: fn(&Vm, &CallFrame) -> Option<(usize, u32)>,
    pub debugger_did_parse: fn(&Cx, &SourceProvider, Option<&CompileError>),
    pub debugger_will_unwind: fn(&Vm, &CallFrame),
    pub debugger_did_unwind_frame: fn(&Vm, &CallFrame),
    pub visit_roots: fn(&Vm, &mut Visitor),
    /// With the words on the stack that point into executable memory.
    pub visit_code_on_stack: fn(&Vm, &[usize], &mut Visitor),
    pub reconcile_weak_references: fn(&Vm, &Survivors),
    pub did_collect: fn(&Vm, Scope),
}

/// A function of the standard library that is written in JavaScript.
pub struct BuiltinSource {
    pub name: &'static str,
    /// Of a function expression in parentheses, in `Hooks::builtins_text`.
    pub range: Range<u32>,
    pub kind: FunctionKind,
    pub visibility: Visibility,
    pub intrinsic: Intrinsic,
}
