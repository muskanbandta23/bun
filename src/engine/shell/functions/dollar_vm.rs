//! `$vm`: looks inside the engine.

use bun_engine_vm::options;

use super::{coverage, define_functions, heap, io, object_with, scripts, tiers};
use crate::prelude::*;

const FUNCTIONS: Functions = &[
    ("abort", abort, 0),
    ("crash", abort, 0),
    ("gc", heap::gc, 0),
    ("edenGC", heap::eden_gc, 0),
    ("noInline", tiers::no_inline, 1),
    ("print", io::print_err, 1),
    ("createGlobalObject", scripts::create_global_object, 0),
    ("useJIT", uses_jit, 0),
    ("useDFGJIT", uses_quick_optimizer, 0),
    ("useFTLJIT", uses_full_optimizer, 0),
    ("isWasmSupported", is_wasm_supported, 0),
];

pub(super) fn create(cx: &Cx) -> Gc<JsObject> {
    let object = object_with(cx, FUNCTIONS, Attributes::NONE);
    define_functions(cx, &object, coverage::VM_FUNCTIONS, Attributes::NONE);
    object
}

#[native_abi]
fn abort(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    std::process::abort()
}

#[native_abi]
fn uses_jit(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(options().use_jit.into())
}

#[native_abi]
fn uses_quick_optimizer(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok((options().use_jit && options().use_quick_optimizer).into())
}

#[native_abi]
fn uses_full_optimizer(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok((options().use_jit && options().use_full_optimizer).into())
}

#[native_abi]
fn is_wasm_supported(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(options().use_wasm.into())
}
