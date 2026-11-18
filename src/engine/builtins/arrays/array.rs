use bun_engine_vm::{JsArray, JsFunction, JsObjectInit, RealmSlot};

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::realm_slot(names::values, RealmSlot::array_prototype_values),
    StaticProperty::realm_slot(symbols::iterator, RealmSlot::array_prototype_values),
]);

#[cell(name = "Array", shared_type = DerivedArray, static_table = PROTOTYPE_PROPERTIES)]
struct ArrayPrototype {
    base: JsArray,
}

static_table!(static ITERATOR_PROTOTYPE_PROPERTIES = []);

define_object!(
    ArrayIteratorPrototype,
    "Array Iterator",
    ITERATOR_PROTOTYPE_PROPERTIES
);

pub(crate) fn initialize_prototype(cx: &Cx) {
    let (global, parent) = (cx.global(), cx.object_prototype().into());
    let spec = ShapeSpec::of::<ArrayPrototype>().array_without_elements();
    let shape = Shape::create(cx, Some(global), parent, spec);
    let base = JsArray::init(JsObjectInit::new(cx, shape));
    let prototype = cx.allocate::<ArrayPrototype>(ArrayPrototypeInit { base });
    global.init_array_prototype(prototype.upcast());
    global.create_array_shapes(cx);
    let name = names::values.to_identifier().into_string();
    let values = JsFunction::native_with_intrinsic(cx, 0, &name, values, Intrinsic::ArrayValues);
    global.init_array_prototype_values(values);
    let parent = cx.iterator_prototype().into();
    global.init_array_iterator_prototype(ArrayIteratorPrototype::create(cx, parent));
}

#[native_abi]
fn values(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(Value::UNDEFINED)
}
