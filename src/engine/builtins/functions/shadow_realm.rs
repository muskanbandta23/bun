//! https://tc39.es/proposal-shadowrealm/

use bun_engine_text::StringRef;
use bun_engine_vm::{
    Attributes, ErrorInstance, ErrorType, GlobalObject, JsObjectInit, JsRemoteFunction, Thrown, errors,
    execute_indirect_eval, origin_of_caller, prepare_indirect_eval,
};

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::function(names::evaluate, evaluate, 1),
    StaticProperty::function(names::importValue, import_value, 2),
    StaticProperty::string(symbols::toStringTag, names::ShadowRealm),
]);

define_object!(ShadowRealmPrototype, "ShadowRealm", PROTOTYPE_PROPERTIES);

static_table!(static CONSTRUCTOR_PROPERTIES = []);

define_constructor!(
    ShadowRealmConstructor,
    CONSTRUCTOR_PROPERTIES,
    call,
    construct
);

#[cell(name = "ShadowRealm", cell_type = ShadowRealm)]
struct ShadowRealmObject {
    base: JsObject,
    inner: Gc<GlobalObject>,
}

pub(super) fn initialize(cx: &Cx) {
    let global = cx.global();
    let prototype = ShadowRealmPrototype::create(cx, cx.object_prototype().into());
    let (parent, name) = (cx.function_prototype(), names::ShadowRealm);
    let constructor = ShadowRealmConstructor::create(cx, parent, 0, name, &prototype);
    let spec = ShapeSpec::of::<ShadowRealmObject>();
    global.init_shadow_realm_shape(Shape::create(cx, Some(global), prototype.into(), spec));
    global.init_shadow_realm_constructor(constructor);
}

#[native_abi]
fn call(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Err(errors::throw_called_without_new(cx, "ShadowRealm"))
}

#[native_abi]
fn construct(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (new_target, callee) = (frame.new_target(), frame.callee());
    let shape = cx.shape_from_new_target(new_target, &callee, GlobalObject::shadow_realm_shape)?;
    let inner = (cx.global().hooks().derive_shadow_realm)(cx)?;
    if let Some(message) = cx.global().eval_disabled_message().borrow().clone() {
        inner.eval_disabled_message().replace(Some(message));
    }
    let base = JsObjectInit::new(cx, shape);
    Ok(cx
        .allocate::<ShadowRealmObject>(ShadowRealmObjectInit { base, inner })
        .into())
}

fn this_realm(cx: &Cx, frame: &CallFrame, method: &str) -> JsResult<Gc<GlobalObject>> {
    match frame.this_value().downcast::<ShadowRealmObject>() {
        Some(realm) => Ok(realm.inner),
        None => Err(cx.throw_type_error(format_args!(
            "`%ShadowRealm%.{method} requires that |this| be a ShadowRealm instance"
        ))),
    }
}

/// CreateTypeErrorCopy, and throws it.
#[cold]
fn throw_type_error_copy(cx: &mut Cx, error: Value) -> JsResult<Thrown> {
    let message = match error.as_object() {
        Some(error) => (error.get_direct_with_attributes(names::message))
            .filter(|found| !found.1.intersects(Attributes::ANY_ACCESSOR) && found.0.is_primitive())
            .map(|found| found.0),
        None => Some(error),
    };
    let message = match message {
        Some(message) => message.to_string_ref(cx)?,
        None => StringRef::from_literal("Error encountered during evaluation"),
    };
    Ok(cx.throw_type_error(message))
}

#[native_abi]
fn evaluate(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let realm = this_realm(cx, frame, "evaluate")?;
    let Some(text) = frame.argument(0).as_string() else {
        return Err(cx.throw_type_error(
            "`%ShadowRealm%.evaluate requires that the |sourceText| argument be a string",
        ));
    };
    let text = text.to_string_ref(cx)?;
    cx.ensure_can_compile_strings()?;
    let origin = origin_of_caller(cx);
    let executable = match prepare_indirect_eval(cx.in_realm(realm), text, origin.as_deref()) {
        Ok(executable) => executable,
        Err(thrown) => {
            let error = cx.catch_value(thrown)?;
            return Err(match error.downcast::<ErrorInstance>() {
                Some(error) if error.error_type() == ErrorType::SyntaxError => {
                    let message = error.sanitized_message(cx)?.unwrap_or_default();
                    cx.throw_syntax_error(message)
                }
                _ => throw_type_error_copy(cx, error)?,
            });
        }
    };
    match execute_indirect_eval(cx.in_realm(realm), executable) {
        Ok(result) => JsRemoteFunction::wrap(cx, cx.global(), result),
        Err(thrown) => {
            let error = cx.catch_value(thrown)?;
            Err(throw_type_error_copy(cx, error)?)
        }
    }
}

#[native_abi]
fn import_value(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    this_realm(cx, frame, "importValue")?;
    Err(cx.throw_type_error(errors::TYPE_ERROR))
}
