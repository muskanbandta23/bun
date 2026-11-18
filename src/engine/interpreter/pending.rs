//! Stand-ins for run-time support that is yet to come.

use core::mem::MaybeUninit;

use bun_engine_bytecode::ImportPhase;
use bun_engine_vm::{CodeBlock, Cx, Gc, JsCell, JsResult, NoGc, Value};

pub(crate) fn dynamic_import(
    cx: &mut Cx,
    _: &CodeBlock,
    _: Value,
    _: Value,
    _: Gc<JsCell>,
    _: ImportPhase,
) -> JsResult<Value> {
    Err(cx.throw_type_error("Not supported: import()"))
}

pub(crate) fn argument_list_length(_: Value, _: u32) -> usize {
    0
}

pub(crate) fn load_argument_list(_: &NoGc<'_>, _: Value, _: u32, _: &mut [MaybeUninit<Value>]) {}

pub(crate) fn spread_length(_: &[Option<Value>]) -> usize {
    0
}

pub(crate) fn load_spread_arguments(_: &[Option<Value>], _: &mut [MaybeUninit<Value>]) {}
