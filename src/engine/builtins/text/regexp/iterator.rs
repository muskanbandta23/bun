use bun_engine_vm::RegExpStringIterator;

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = [
    StaticProperty::function(names::next, next, 0)
        .with_intrinsic(Intrinsic::RegExpStringIteratorNext),
    StaticProperty::string(symbols::toStringTag, names::regExpStringIteratorTag),
]);

define_object!(Prototype, "RegExp String Iterator", PROTOTYPE_PROPERTIES);

pub(super) fn initialize(cx: &Cx) {
    let (global, spec) = (cx.global(), ShapeSpec::of::<RegExpStringIterator>());
    let prototype = Prototype::create(cx, cx.iterator_prototype().into());
    let shape = Shape::create(cx, Some(global), prototype.into(), spec);
    global.init_regexp_string_iterator_shape(shape);
}

#[native_abi]
fn next(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let this = frame.this_value();
    let Some(iterator) = this.downcast::<RegExpStringIterator>() else {
        return Err(cx.throw_type_error(if this.is_object() {
            "%RegExpStringIteratorPrototype%.next requires |this| to be a RegExp String Iterator instance"
        } else {
            "%RegExpStringIteratorPrototype%.next requires |this| to be an Object"
        }));
    };
    let result = iterator.next_match(cx)?;
    let value = result.map_or_default(Into::into);
    Ok(cx.new_iterator_result(value, result.is_none()).into())
}
