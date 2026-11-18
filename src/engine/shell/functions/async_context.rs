//! What an embedder builds `AsyncLocalStorage` on.

use crate::prelude::*;

pub(super) const FUNCTIONS: Functions = &[
    ("asyncContext", async_context, 0),
    ("setAsyncContext", set_async_context, 1),
];

#[native_abi]
fn async_context(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    cx.start_tracking_async_context();
    Ok(cx.current_async_context())
}

#[native_abi]
fn set_async_context(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    cx.start_tracking_async_context();
    cx.async_context().fields().set(0, frame.argument(0));
    Ok(Value::UNDEFINED)
}
