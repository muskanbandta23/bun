use bun_engine_vm::StringObject;

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::realm_slot(symbols::iterator, bun_engine_vm::RealmSlot::string_prototype_iterator),
]);

#[cell(name = "String", shared_type = DerivedStringObject, static_table = PROTOTYPE_PROPERTIES)]
struct StringPrototype {
    base: StringObject,
}

pub(crate) fn initialize_prototype(cx: &Cx) {
    let (global, parent) = (cx.global(), cx.object_prototype().into());
    let spec = ShapeSpec::of::<StringPrototype>();
    let shape = Shape::create(cx, Some(global), parent, spec);
    let empty: Gc<JsString> = cx.small_strings.empty();
    let base = StringObject::init(cx, shape, empty);
    let prototype = cx.allocate::<StringPrototype>(StringPrototypeInit { base });
    global.init_string_prototype(prototype.upcast());
}
