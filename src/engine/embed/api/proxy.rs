use bun_engine_macros::export;
use bun_engine_text::PropertyName;
use bun_engine_vm::{Cx, Gc, JsObject, JsResult, ProxyObject, Value};

#[export]
fn proxy_create(cx: &Cx, target: Value, handler: Value) -> JsResult<Gc<ProxyObject>> {
    ProxyObject::create(cx, target, handler)
}

#[export]
fn proxy_find_trap(
    cx: &mut Cx,
    handler: Gc<JsObject>,
    trap: PropertyName<'_>,
) -> JsResult<Option<Gc<JsObject>>> {
    Ok(ProxyObject::find_trap(cx, handler, trap)?.and_then(Value::as_object))
}

#[export]
fn proxy_validate_get_result(
    cx: &mut Cx,
    result: Value,
    target: &JsObject,
    name: PropertyName<'_>,
) -> JsResult<()> {
    ProxyObject::validate_get_result(cx, result, target, name)
}
