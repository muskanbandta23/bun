use bun_engine_bytecode::{BuiltinFunction, FunctionKind, FunctionRef, Visibility};
use bun_engine_bytecode::{CoverageBlockIndex, DebugHook};
use bun_engine_text::{Identifier, PropertyName};

use crate::heap::{Scope, Survivors};
use crate::{
    BuiltinId, CallFrame, CodeBlock, CompileError, Cx, Gc, GlobalObject, Intrinsic, JsCell,
    JsObject, JsResult, PropertyCondition, RealmSlot, RealmWatchpoint, SourceProvider, Value,
    Visitor, Vm, WasmFrame, WasmNames,
};

/// What this crate needs from those above it.
pub struct Hooks {
    /// The entry of an executable whose code is interpreted.
    pub interpreter_entry: usize,
    /// Where code that is not optimized goes on at the handler at that offset.
    pub catch_address: fn(&CodeBlock, u32) -> usize,
    pub native_call_entry: fn(&Vm, Intrinsic) -> usize,
    pub jettison: fn(&Vm, &CodeBlock),
    /// The code that baseline code jumps to for the case, or zero if there is no such tier.
    pub access_handler: fn(&Vm, &crate::ic::AccessCase, crate::ic::Key) -> usize,
    /// The counter of the code block has run out, at the entry or at the head of a loop. Returns where to go on in the
    /// same frame, or zero.
    pub tier_up: fn(&Vm, &CodeBlock, &CallFrame, Option<u32>) -> usize,
    /// The last argument is the scope.
    pub debug: fn(&mut Cx, &CallFrame, DebugHook, crate::Value) -> crate::JsResult<()>,
    pub cover: fn(&Vm, &CodeBlock, CoverageBlockIndex, &crate::code::metadata::Word),
    /// Sets the eager slots but `global_this`.
    pub initialize_realm: fn(&Cx),
    pub initialize_realm_slot: fn(&Cx, RealmSlot),
    /// `None` if it does not hold any more.
    pub realm_watchpoint_conditions: fn(&Cx, RealmWatchpoint) -> Option<Vec<PropertyCondition>>,
    pub did_start_having_a_bad_time: fn(&Vm, &GlobalObject),
    pub did_declare_global_lexical: fn(&Vm, &GlobalObject, PropertyName<'_>),
    pub builtin_source: fn(BuiltinId) -> &'static BuiltinSource,
    /// `toLocaleString` of an instance of a class of `Temporal`, with the locales and the options.
    pub temporal_to_locale_string: fn(&mut Cx, Gc<JsObject>, Value, Value) -> JsResult<Value>,
    /// Of a `WebAssembly.Memory`, by so many pages. Returns whether it has grown.
    pub grow_wasm_memory: fn(&mut Cx, Gc<JsObject>, usize) -> bool,
    pub find_wasm_handler: fn(&Vm, &CallFrame) -> Option<(usize, u32)>,
    /// With the address that the callee returns to, or zero if the frame has called an operation, and the number of
    /// inlined functions that have been visited.
    pub wasm_frame: fn(&Vm, &CallFrame, usize, u32) -> WasmFrame,
    /// Of the function with that index. The cell may have just died.
    pub wasm_names: fn(Gc<JsCell>, u32) -> WasmNames,
    pub debugger_did_parse: fn(&Cx, &SourceProvider, Option<&CompileError>),
    /// With a number that tells the task from all others.
    pub debugger_microtask_event: fn(&Vm, &GlobalObject, crate::MicrotaskEvent, u64),
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
