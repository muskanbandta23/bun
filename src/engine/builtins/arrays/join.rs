use bun_engine_number::NumberText;
use bun_engine_text::{StringBuilder, Text};
use bun_engine_vm::{JsArray, RealmWatchpoint, is_array};

use crate::prelude::*;

fn push_separators(out: &mut StringBuilder, separator: Text<'_>, count: u64) {
    match separator {
        Text::Latin1([]) | Text::Utf16([]) => {}
        Text::Latin1(&[character]) => out.push_repeated(character, count as usize),
        _ => {
            out.reserve((count as usize).saturating_mul(separator.len()));
            for _ in 0..count {
                if out.has_overflowed() {
                    break;
                }
                out.push_text(separator);
            }
        }
    }
}

fn join_with(
    cx: &mut Cx,
    object: &JsObject,
    length: u64,
    separator: Text<'_>,
    mut push: impl FnMut(&mut Cx, &mut StringBuilder, Value) -> JsResult<()>,
) -> JsResult<Gc<JsString>> {
    let mut out = StringBuilder::new();
    let mut index = 0;
    while index < length && !out.has_overflowed() {
        if index != 0 {
            out.push_text(separator);
        }
        let element = object.get_index(cx, index)?;
        index += 1;
        if !element.is_undefined_or_null() {
            push(cx, &mut out, element)?;
        } else {
            let next = object.skip_holes(index..length);
            push_separators(&mut out, separator, next - index);
            index = next;
        }
        cx.check_traps()?;
    }
    cx.finish_string(out)
}

fn push_as_string(cx: &mut Cx, out: &mut StringBuilder, element: Value) -> JsResult<()> {
    match element.as_number() {
        Some(number) => out.push_text(NumberText::from(number).text()),
        None => out.push_text(element.to_string_view(cx)?.text()),
    }
    Ok(())
}

const COMMA: Text<'static> = Text::from_ascii(",");

#[native_abi]
pub(super) fn join(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    object
        .unless_being_joined(cx, |cx| {
            let length = object.length_of_array_like(cx)?;
            match frame.argument(0) {
                Value::UNDEFINED => join_with(cx, &object, length, COMMA, push_as_string),
                separator => {
                    let separator = separator.to_string_view(cx)?;
                    join_with(cx, &object, length, separator.text(), push_as_string)
                }
            }
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
        let join =
            |cx: &mut Cx| join_with(cx, &array, array.length().into(), COMMA, push_as_string);
        return array.unless_being_joined(cx, join).map(Into::into);
    }
    let method = object.get(cx, names::join)?;
    if method.is_callable() {
        return cx.call(method, object.into(), &[]);
    }
    // For the message about a revoked proxy.
    is_array(cx, object.into())?;
    object.to_tagged_string(cx).map(Into::into)
}

#[native_abi]
pub(super) fn to_locale_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    let arguments = [frame.argument(0), frame.argument(1)];
    object
        .unless_being_joined(cx, |cx| {
            let length = object.length_of_array_like(cx)?;
            join_with(cx, &object, length, COMMA, |cx, out, element| {
                let method = element.get(cx, names::toLocaleString)?;
                if !method.is_callable() {
                    return Err(cx.throw_type_error("toLocaleString is not callable"));
                }
                let result = cx.call(method, element, &arguments)?;
                out.push_text(result.to_string_view(cx)?.text());
                Ok(())
            })
        })
        .map(Into::into)
}
