//! The functions of the global object that escape text.

use bun_engine_text::{PropertyName, StringRef, Text, UriError, UriSet};
use bun_engine_vm::{ErrorType, JsFunction, RealmSlot};

use crate::prelude::*;

pub(super) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    let make = |name: PropertyName<'_>, function| {
        JsFunction::native(cx, 1, &name.to_identifier().into_string(), function)
    };
    let global = cx.global();
    match slot {
        RealmSlot::escape_function => global.init_escape_function(make(names::escape, escape)),
        RealmSlot::unescape_function => {
            global.init_unescape_function(make(names::unescape, unescape));
        }
        RealmSlot::decode_uri_function => {
            global.init_decode_uri_function(make(names::decodeURI, decode_uri));
        }
        RealmSlot::decode_uri_component_function => {
            let function = make(names::decodeURIComponent, decode_uri_component);
            global.init_decode_uri_component_function(function);
        }
        RealmSlot::encode_uri_function => {
            global.init_encode_uri_function(make(names::encodeURI, encode_uri));
        }
        RealmSlot::encode_uri_component_function => {
            let function = make(names::encodeURIComponent, encode_uri_component);
            global.init_encode_uri_component_function(function);
        }
        _ => return false,
    }
    true
}

#[native_abi]
fn escape(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let string = frame.argument(0).to_string(cx)?;
    string.converted(cx, |text| text.escape()).map(Into::into)
}

#[native_abi]
fn unescape(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let string = frame.argument(0).to_string(cx)?;
    string.converted(cx, |text| text.unescape()).map(Into::into)
}

#[inline(always)]
fn convert_uri(
    cx: &mut Cx,
    frame: &CallFrame,
    convert: impl FnOnce(Text<'_>) -> Result<Option<StringRef>, UriError>,
) -> JsResult<Value> {
    let string = frame.argument(0).to_string(cx)?;
    match convert(string.view(cx)?.text()) {
        Ok(None) => Ok(string.into()),
        Ok(Some(converted)) => Ok(JsString::new(cx, converted).into()),
        Err(UriError::OutOfMemory) => Err(cx.throw_out_of_memory()),
        Err(error) => Err(cx.throw_error(ErrorType::UriError, error.message())),
    }
}

#[native_abi]
fn decode_uri(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    convert_uri(cx, frame, |text| text.decode_uri(UriSet::Uri))
}

#[native_abi]
fn decode_uri_component(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    convert_uri(cx, frame, |text| text.decode_uri(UriSet::Component))
}

#[native_abi]
fn encode_uri(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    convert_uri(cx, frame, |text| text.encode_uri(UriSet::Uri))
}

#[native_abi]
fn encode_uri_component(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    convert_uri(cx, frame, |text| text.encode_uri(UriSet::Component))
}
