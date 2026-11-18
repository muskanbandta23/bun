use bun_engine_vm::{JsArray, RealmWatchpoint};

use crate::prelude::*;

#[native_abi]
pub(super) fn join(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    object
        .unless_being_joined(cx, |cx| {
            let length = object.length_of_array_like(cx)?;
            let separator = match frame.argument(0) {
                Value::UNDEFINED => cx.small_strings.single_character(b','),
                separator => separator.to_string(cx)?,
            };
            object.join_elements(cx, length, separator)
        })
        .map(Into::into)
}

#[native_abi]
pub(super) fn to_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    if let Some(array) = object.downcast::<JsArray>()
        && let Some(global) = array.realm()
        && array.inherits_from_array_prototype(&global, names::join)
        && global.holds(cx, RealmWatchpoint::ArrayJoin)
    {
        let comma = cx.small_strings.single_character(b',');
        let join = |cx: &mut Cx| array.join_elements(cx, array.length().into(), comma);
        return array.unless_being_joined(cx, join).map(Into::into);
    }
    let mut method = object.get(cx, names::join)?;
    if !method.is_callable() {
        method = cx.object_prototype_to_string().into();
    }
    cx.call(method, object.into(), &[])
}

#[native_abi]
pub(super) fn to_locale_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    let arguments = [frame.argument(0), frame.argument(1)];
    object
        .unless_being_joined(cx, |cx| {
            let length = object.length_of_array_like(cx)?;
            let comma = cx.small_strings.single_character(b',');
            object.join_elements_with(cx, length, comma, |cx, element| {
                let method = element.get(cx, names::toLocaleString)?;
                if !method.is_callable() {
                    return Err(cx.throw_type_error("toLocaleString is not callable"));
                }
                cx.call(method, element, &arguments)?.to_string(cx)
            })
        })
        .map(Into::into)
}
