use bun_engine_vm::{JsFinalObject, JsFunction, JsObjectInit, TypeFlags};

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::realm_slot(names::valueOf, bun_engine_vm::RealmSlot::object_prototype_value_of),
]);

#[cell(name = "Object", static_table = PROTOTYPE_PROPERTIES,
    flags = TypeFlags::IS_IMMUTABLE_PROTOTYPE_EXOTIC_OBJECT)]
struct ObjectPrototype {
    base: JsObject,
}

pub(crate) fn initialize_prototype(cx: &Cx) {
    let (global, spec) = (cx.global(), ShapeSpec::of::<ObjectPrototype>());
    let shape = Shape::create(cx, Some(global), Value::NULL, spec);
    let base = JsObjectInit::new(cx, shape);
    let prototype = cx.allocate::<ObjectPrototype>(ObjectPrototypeInit { base });
    let capacity = JsFinalObject::DEFAULT_INLINE_CAPACITY;
    let shape = |prototype| JsFinalObject::create_shape(cx, global, prototype, capacity);
    global.init_object_prototype(prototype.upcast());
    global.init_object_shape(shape(prototype.into()));
    global.init_null_prototype_object_shape(shape(Value::NULL));
}

pub(crate) fn initialize_value_of(cx: &Cx) {
    let name = names::valueOf.to_identifier().into_string();
    let function = JsFunction::native(cx, 0, &name, value_of);
    cx.global().init_object_prototype_value_of(function);
}

#[native_abi]
fn value_of(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    frame.this_value().to_object(cx).map(Into::into)
}
