use bun_engine_vm::JsStringIterator;

use super::{TYPE_ERROR, this_string};
use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::function(names::next, next, 0).with_intrinsic(Intrinsic::StringIteratorNext),
    StaticProperty::string(symbols::toStringTag, names::stringIteratorTag),
]);

define_object!(
    StringIteratorPrototype,
    "String Iterator",
    PROTOTYPE_PROPERTIES
);

pub(super) fn initialize(cx: &Cx) {
    let (global, spec) = (cx.global(), ShapeSpec::of::<JsStringIterator>());
    let prototype = StringIteratorPrototype::create(cx, cx.iterator_prototype().into());
    global.init_string_iterator_shape(Shape::create(cx, Some(global), prototype.into(), spec));
    global.init_string_iterator_prototype(prototype);
}

#[native_abi]
pub(super) fn iterator(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let string = this_string(cx, frame, TYPE_ERROR)?;
    Ok(JsStringIterator::create(cx, string).into())
}

#[native_abi]
fn next(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(iterator) = frame.this_value().downcast::<JsStringIterator>() else {
        return Err(cx.throw_type_error(
            "%StringIteratorPrototype%.next requires that |this| be a String Iterator instance",
        ));
    };
    let value = iterator.next(cx)?;
    Ok((cx.new_iterator_result(value.map_or_default(Into::into), value.is_none())).into())
}
