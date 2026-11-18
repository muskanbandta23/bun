//! The functions that wait for the one that is running.

use crate::{Gc, JsObject, StackFrame, Value};

pub(super) fn async_function_in(_: Value) -> Option<Gc<JsObject>> {
    None
}

pub(super) fn frames_awaiting(_: Gc<JsObject>) -> impl Iterator<Item = StackFrame> {
    core::iter::empty()
}
