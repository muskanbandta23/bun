//! The functions of the global object that have to do with numbers, and those that only built-ins see.

use bun_engine_bytecode::Visibility::{self, Private, Public};
use bun_engine_number::parse_int_of_number;
use bun_engine_text::PropertyName;
use bun_engine_vm::call::native_not_a_constructor;
use bun_engine_vm::{JsFunction, NativeFunction, RealmSlot};

use super::math;
use crate::prelude::*;

pub(super) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    let make = |name: PropertyName<'_>,
                function: NativeFunction,
                length,
                intrinsic,
                visibility: Visibility| {
        let (name, constructor) = (name.to_identifier().into_string(), native_not_a_constructor);
        JsFunction::native_with(
            cx,
            length,
            &name,
            function,
            constructor,
            intrinsic,
            visibility,
        )
    };
    let global = cx.global();
    match slot {
        RealmSlot::is_nan_function => {
            let intrinsic = Intrinsic::GlobalIsNaN;
            global.init_is_nan_function(make(names::isNaN, is_nan, 1, intrinsic, Public));
        }
        RealmSlot::is_finite_function => {
            let intrinsic = Intrinsic::GlobalIsFinite;
            global.init_is_finite_function(make(names::isFinite, is_finite, 1, intrinsic, Public));
        }
        RealmSlot::parse_int_function => {
            let intrinsic = Intrinsic::ParseInt;
            global.init_parse_int_function(make(names::parseInt, parse_int, 2, intrinsic, Public));
        }
        RealmSlot::parse_float_function => {
            let function = make(names::parseFloat, parse_float, 1, Intrinsic::None, Public);
            global.init_parse_float_function(function);
        }
        RealmSlot::private_min => {
            global.init_private_min(make(names::min, math::min, 0, Intrinsic::Min, Private));
        }
        RealmSlot::private_to_length => {
            let function = make(names::toLength, to_length, 1, Intrinsic::ToLength, Private);
            global.init_private_to_length(function);
        }
        RealmSlot::private_to_integer_or_infinity => {
            let (name, intrinsic) = (names::toIntegerOrInfinity, Intrinsic::ToIntegerOrInfinity);
            let function = make(name, to_integer_or_infinity, 1, intrinsic, Private);
            global.init_private_to_integer_or_infinity(function);
        }
        _ => return false,
    }
    true
}

#[native_abi]
fn is_nan(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.argument(0).to_number(cx)?.is_nan().into())
}

#[native_abi]
fn is_finite(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.argument(0).to_number(cx)?.is_finite().into())
}

#[native_abi]
fn parse_int(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (value, radix) = (frame.argument(0), frame.argument(1));
    if radix.is_undefined_or_null() || radix == Value::int32(10) {
        if value.is_int32() {
            return Ok(value);
        }
        if let Some(result) = value.as_double().and_then(parse_int_of_number) {
            return Ok(result.into());
        }
    }
    let string = value.to_string(cx)?;
    let radix = radix.to_int32(cx)?;
    string.parse_int(cx, radix).map(Value::number)
}

#[native_abi]
fn parse_float(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let value = frame.argument(0);
    if value.is_number() {
        let is_zero = value.as_double() == Some(0.0);
        return Ok(if is_zero { Value::ZERO } else { value });
    }
    let view = value.to_string_view(cx)?;
    Ok(bun_engine_number::parse_float(view.text()).into())
}

#[native_abi]
fn to_length(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    frame.argument(0).to_length(cx).map(Into::into)
}

#[native_abi]
fn to_integer_or_infinity(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let value = frame.argument(0);
    if value.is_int32() {
        return Ok(value);
    }
    value.to_integer_or_infinity(cx).map(Value::number)
}
