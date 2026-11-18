use bun_engine_bytecode::{BuiltinFunction, FunctionKind, FunctionRef, Visibility};
use bun_engine_text::{Identifier, PropertyName};

use crate::heap::{Scope, Survivors};
use crate::{
    BuiltinId, CallFrame, CodeBlock, Cx, GlobalObject, Intrinsic, PropertyCondition, RealmSlot,
    RealmWatchpoint, Visitor, Vm,
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
    pub builtin_source: fn(BuiltinId) -> &'static BuiltinSource,
    pub find_wasm_handler: fn(&Vm, &CallFrame) -> Option<(usize, u32)>,
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
    /// A strict function expression without a name, in parentheses.
    pub text: &'static str,
    pub parameters_start: u32,
    pub length: u32,
    pub is_async: bool,
    pub is_generator: bool,
    pub is_constructor: bool,
    pub is_always_inlined: bool,
    pub visibility: Visibility,
    pub intrinsic: Intrinsic,
}

impl BuiltinSource {
    pub(crate) fn to_function(&self) -> FunctionRef {
        BuiltinFunction {
            name: Identifier::from_literal(self.name),
            range: 1..self.text.len() as u32 - 1,
            parameters_start: self.parameters_start,
            length: self.length,
            kind: if self.is_constructor {
                FunctionKind::NakedConstructor
            } else {
                FunctionKind::Method
            },
            is_strict: true,
            is_async: self.is_async,
            is_generator: self.is_generator,
            visibility: self.visibility,
            has_positions: false,
        }
        .into_function()
    }
}
