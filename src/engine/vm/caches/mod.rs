//! Caches of property lookups that all code shares.

use bun_engine_text::PropertyName;

use crate::{Cx, Gc, JsObject, JsResult, Value};

pub fn get_megamorphic(cx: &mut Cx, base: Value, name: PropertyName<'_>) -> JsResult<Value> {
    base.get(cx, name)
}

pub fn has_megamorphic(cx: &mut Cx, base: Gc<JsObject>, name: PropertyName<'_>) -> JsResult<bool> {
    base.has_property(cx, name)
}

pub fn put_megamorphic(
    cx: &mut Cx,
    base: Gc<JsObject>,
    name: PropertyName<'_>,
    value: Value,
    is_strict: bool,
) -> JsResult<()> {
    base.set(cx, name, value, is_strict).map(|_| ())
}

#[derive(Default)]
pub(crate) struct State {}

impl State {
    pub(crate) fn did_wrap_epoch(&self) {}
}

impl crate::services::Service for State {}
