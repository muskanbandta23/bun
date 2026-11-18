//! Between values and JSON, without asking them how they would like to look.

use bun_engine_text::Identifier;
use bun_engine_vm::{
    Cx, DontEnumMode, JsArray, JsResult, JsString, PropertyNameMode, Value, ValueVec,
};

use crate::protocol::{Json, Object};

/// `None` if it has a symbol or a BigInt in it, or nests deeper than `depth`.
pub(super) fn to_json(cx: &mut Cx, value: Value, depth: u32) -> JsResult<Option<Json>> {
    let Some(depth) = depth.checked_sub(1) else {
        return Ok(None);
    };
    let Some(object) = value.as_object() else {
        return primitive_to_json(cx, value);
    };
    cx.check_stack()?;
    if let Some(array) = object.downcast::<JsArray>() {
        let mut elements = Vec::new();
        for index in 0..array.length() {
            cx.check_traps()?;
            let element = array.get_index(cx, index.into())?;
            let Some(element) = to_json(cx, element, depth)? else {
                return Ok(None);
            };
            if elements.try_reserve(1).is_err() {
                return Err(cx.throw_out_of_memory());
            }
            elements.push(element);
        }
        return Ok(Some(elements.into()));
    }
    let mut members = Object::default();
    let names = object.own_keys(cx, PropertyNameMode::Strings, DontEnumMode::Exclude)?;
    for name in names.iter() {
        let member = object.get(cx, name.name())?;
        let Some(member) = to_json(cx, member, depth)? else {
            return Ok(None);
        };
        members.add_named(name.string().clone(), member);
    }
    Ok(Some(members.into()))
}

pub(super) fn primitive_to_json(cx: &Cx, value: Value) -> JsResult<Option<Json>> {
    if let Some(string) = value.as_string() {
        return Ok(Some(string.to_string_ref(cx)?.into()));
    }
    Ok((value.as_number().map(Json::from))
        .or_else(|| value.as_boolean().map(Json::from))
        .or_else(|| value.is_undefined_or_null().then_some(Json::Null)))
}

pub(super) fn from_json(cx: &mut Cx, json: &Json) -> JsResult<Value> {
    cx.check_stack()?;
    Ok(match json {
        Json::Null => Value::NULL,
        &Json::Boolean(value) => value.into(),
        &Json::Number(value) => Value::number(value),
        Json::Literal(value) => JsString::from_literal(cx, value).into(),
        Json::String(value) => JsString::new(cx, value.clone()).into(),
        Json::Array(elements) => {
            let mut values = ValueVec::new();
            for element in elements {
                values.push(from_json(cx, element)?);
            }
            cx.new_array_from(&values)?.into()
        }
        Json::Object(members) => {
            let object = cx.new_object();
            for (name, member) in members.members() {
                let member = from_json(cx, member)?;
                object.create_data_property(cx, Identifier::new(name).name(), member, false)?;
            }
            object.into()
        }
    })
}
