use core::ffi::c_void;
use core::ptr::NonNull;
use std::rc::Rc;

use bun_engine_bytecode::{FunctionRef, Instrumentation, Script, StaticScope};
use bun_engine_macros::export;
use bun_engine_vm::scope::static_scope_of;
use bun_engine_vm::{
    CompileError, Cx, FunctionExecutable, Gc, JsCell, JsFunction, JsResult, SourceCode, Value, Vm,
    evaluate_compiled_script,
};

use super::scope::can_close_over;
use crate::abi::returned_as_pointer;

/// Whether `take` has been called. There is no serialized form of code yet.
#[export]
unsafe fn bytecode_generate(
    _vm: &Vm,
    _source: &SourceCode,
    _is_module: bool,
    _depth: u32,
    _context: *mut c_void,
    _take: unsafe extern "C" fn(*mut c_void, *const u8, usize),
) -> bool {
    false
}

#[export]
fn bytecode_is_usable(_vm: &Vm, _source: &SourceCode, _is_module: bool, _bytes: &[u8]) -> bool {
    false
}

#[export]
fn code_cache_max_entries(vm: &Vm) -> usize {
    vm.code_cache_max_entries()
}

#[export]
fn code_cache_set_max_entries(vm: &Vm, count: usize) {
    vm.set_code_cache_max_entries(count);
}

struct CompiledScript {
    source: SourceCode,
    script: Rc<Script>,
    outer: Option<Rc<StaticScope>>,
    instrumentation: Instrumentation,
}

/// An `Rc` that points at its value.
#[repr(transparent)]
struct CompiledScriptRef(NonNull<CompiledScript>);

returned_as_pointer! {
    CompiledScriptRef,
}

impl CompiledScript {
    fn compile(
        cx: &Cx,
        source: &SourceCode,
        scope: Option<Gc<JsCell>>,
    ) -> Result<CompiledScriptRef, CompileError> {
        let outer = scope.and_then(static_scope_of);
        let compiled = Rc::new(CompiledScript {
            source: source.clone(),
            script: cx.compile_script(&source.provider, outer.as_ref())?,
            outer,
            instrumentation: cx.global().instrumentation(cx),
        });
        // SAFETY: points into an allocation.
        let pointer = unsafe { NonNull::new_unchecked(Rc::into_raw(compiled).cast_mut()) };
        Ok(CompiledScriptRef(pointer))
    }
}

#[export]
unsafe fn compiled_script_retain(compiled: *const CompiledScript) {
    // SAFETY: all of them are in an `Rc`.
    unsafe { Rc::increment_strong_count(compiled) }
}

#[export]
unsafe fn compiled_script_release(compiled: *const CompiledScript) {
    // SAFETY: a reference that C++ holds.
    unsafe { Rc::decrement_strong_count(compiled) }
}

#[export]
fn compiled_script_is_usable_in(compiled: &CompiledScript, cx: &Cx) -> bool {
    compiled.instrumentation == cx.global().instrumentation(cx)
}

#[export]
fn compiled_script_create_first_function(
    compiled: &CompiledScript,
    cx: &Cx,
    scope: Gc<JsCell>,
) -> Option<Gc<JsFunction>> {
    let functions = &compiled.script.code.functions;
    let (index, function) = ((0..).zip(&**functions)).min_by_key(|it| it.1.range().start)?;
    if !can_close_over(function, scope) {
        return None;
    }
    let (provider, info) = (
        compiled.source.provider.clone(),
        FunctionRef::new(Rc::clone(functions), index),
    );
    let executable = FunctionExecutable::create(cx, provider, None, info);
    // SAFETY: just checked.
    Some(unsafe { JsFunction::create(cx, executable, scope) })
}

/// In the realm of `this`.
#[export]
fn compiled_script_evaluate(
    compiled: &CompiledScript,
    cx: &mut Cx,
    this: Value,
) -> JsResult<Value> {
    assert!(compiled.outer.is_none());
    let this = this.to_sloppy_this(cx);
    let cx = cx.in_realm_of(&this);
    cx.check_stack()?;
    let (source, script) = (compiled.source.clone(), Rc::clone(&compiled.script));
    // SAFETY: compiled from that text, for the global scope.
    unsafe { evaluate_compiled_script(cx, &source, script, cx.global().cell(), this.into()) }
}
