use bun_engine_text::PropertyName;
use bun_engine_vm::{JsFinalObject, JsFunctionWithFields, ProxyObject, errors};

use crate::prelude::*;

static_table!(static PROPERTIES = [
    StaticProperty::position_of(names::length),
    StaticProperty::position_of(names::name),
    StaticProperty::function(names::revocable, revocable, 2),
]);

#[cell(name = "Proxy", static_table = PROPERTIES)]
struct ProxyConstructor {
    base: InternalFunction,
}

pub(super) fn initialize(cx: &Cx) {
    let (global, parent) = (cx.global(), cx.function_prototype().into());
    let spec = ShapeSpec::of::<ProxyConstructor>();
    let shape = Shape::create_dictionary(cx, Some(global), parent, spec);
    let constructor = cx.allocate::<ProxyConstructor>(ProxyConstructorInit {
        base: InternalFunction::init(cx, shape, call, Some(construct)),
    });
    constructor.define_length_and_name(cx, 2, JsString::from_name(cx, names::Proxy));
    global.init_proxy_constructor(constructor.upcast());
}

pub(super) fn initialize_revocable_shape(cx: &Cx) {
    let names = [names::proxy, names::revoke].map(PropertyName::to_identifier);
    let shape = Shape::for_object_literal(cx, &names, names.len());
    cx.global().init_revocable_proxy_shape(shape);
}

#[native_abi]
fn call(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Err(errors::throw_called_without_new(cx, "Proxy"))
}

#[native_abi]
fn construct(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    ProxyObject::create(cx, frame.argument(0), frame.argument(1)).map(Into::into)
}

#[native_abi]
fn revocable(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let &[target, handler, ..] = frame.arguments() else {
        return Err(cx.throw_type_error(
            "Proxy.revocable needs to be called with two arguments: the target and the handler",
        ));
    };
    let proxy = ProxyObject::create(cx, target, handler)?.into();
    let revoke = JsFunctionWithFields::new(cx, 0, revoke, [proxy, Value::UNDEFINED]);
    let shape = cx.revocable_proxy_shape();
    Ok(JsFinalObject::create_with_values(cx, shape, &[proxy, revoke.into()]).into())
}

#[native_abi]
fn revoke(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let fields = frame.callee().cast::<JsFunctionWithFields>();
    if let Some(proxy) = fields.fields().at(0).downcast::<ProxyObject>() {
        proxy.revoke();
        fields.fields().set(0, Value::NULL);
    }
    Ok(Value::UNDEFINED)
}
