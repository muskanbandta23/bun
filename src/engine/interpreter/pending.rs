//! Stand-ins for run-time support that is yet to come.

use bun_engine_bytecode::ImportPhase;
use bun_engine_vm::{CodeBlock, Cx, Gc, JsCell, JsResult, Value};

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
