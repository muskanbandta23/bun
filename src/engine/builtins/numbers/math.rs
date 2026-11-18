use core::f64::consts;

use bun_engine_number::math::{self, Hypot};

use crate::prelude::*;

macro_rules! math_properties {
    ($($name:ident / $length:literal $intrinsic:ident,)*) => {
        static_table!(static PROPERTIES = [
            StaticProperty::number(names::E, consts::E),
            StaticProperty::number(names::LN2, consts::LN_2),
            StaticProperty::number(names::LN10, consts::LN_10),
            StaticProperty::number(names::LOG2E, consts::LOG2_E),
            StaticProperty::number(names::LOG10E, consts::LOG10_E),
            StaticProperty::number(names::PI, consts::PI),
            StaticProperty::number(names::SQRT1_2, consts::FRAC_1_SQRT_2),
            StaticProperty::number(names::SQRT2, consts::SQRT_2),
            StaticProperty::string(symbols::toStringTag, names::Math),
            $(StaticProperty::function(names::$name, $name, $length).with_intrinsic(Intrinsic::$intrinsic),)*
        ]);
    };
}

math_properties! {
    abs / 1 Abs,
    acos / 1 ACos,
    asin / 1 ASin,
    atan / 1 ATan,
    acosh / 1 ACosh,
    asinh / 1 ASinh,
    atanh / 1 ATanh,
    atan2 / 2 None,
    cbrt / 1 Cbrt,
    ceil / 1 Ceil,
    clz32 / 1 Clz32,
    cos / 1 Cos,
    cosh / 1 Cosh,
    exp / 1 Exp,
    expm1 / 1 Expm1,
    floor / 1 Floor,
    fround / 1 FRound,
    hypot / 2 None,
    log / 1 Log,
    log10 / 1 Log10,
    log1p / 1 Log1p,
    log2 / 1 Log2,
    max / 2 Max,
    min / 2 Min,
    pow / 2 Pow,
    random / 0 Random,
    round / 1 Round,
    sign / 1 None,
    sin / 1 Sin,
    sinh / 1 Sinh,
    sqrt / 1 Sqrt,
    tan / 1 Tan,
    tanh / 1 Tanh,
    trunc / 1 Trunc,
    imul / 2 IMul,
    f16round / 1 F16Round,
}

define_object!(MathObject, "Math", PROPERTIES);

pub(super) fn initialize(cx: &Cx) {
    let object = MathObject::create(cx, cx.object_prototype().into());
    cx.global().init_math_object(object);
}

macro_rules! unary_functions {
    ($make:path: $($name:ident)*) => {$(
        #[native_abi]
        fn $name(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
            Ok($make(math::$name(frame.argument(0).to_number(cx)?)))
        }
    )*};
}

unary_functions!(Value::number: abs sign);
unary_functions!(Value::double:
    acos asin atan acosh asinh atanh cbrt cos cosh exp expm1 fround log log10 log1p log2 sin sinh sqrt tan tanh f16round
);

macro_rules! rounding_functions {
    ($($name:ident)*) => {$(
        #[native_abi]
        fn $name(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
            let value = frame.argument(0);
            if value.is_int32() {
                return Ok(value);
            }
            Ok(math::$name(value.to_number(cx)?).into())
        }
    )*};
}

rounding_functions!(ceil floor round trunc);

#[native_abi]
fn atan2(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let y = frame.argument(0).to_number(cx)?;
    let x = frame.argument(1).to_number(cx)?;
    Ok(Value::double(math::atan2(y, x)))
}

#[native_abi]
fn pow(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let base = frame.argument(0).to_number(cx)?;
    let exponent = frame.argument(1).to_number(cx)?;
    Ok(math::pow(base, exponent).into())
}

#[native_abi]
fn clz32(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.argument(0).to_uint32(cx)?.leading_zeros().into())
}

#[native_abi]
fn imul(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let a = frame.argument(0).to_int32(cx)?;
    let b = frame.argument(1).to_int32(cx)?;
    Ok(a.wrapping_mul(b).into())
}

#[native_abi]
fn hypot(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(Value::double(match *frame.arguments() {
        [] => 0.0,
        [a] => a.to_number(cx)?.abs(),
        [a, b] => math::hypot(a.to_number(cx)?, b.to_number(cx)?),
        [a, b, c] => math::hypot3(a.to_number(cx)?, b.to_number(cx)?, c.to_number(cx)?),
        ref arguments => {
            let mut hypot = Hypot::default();
            for argument in arguments {
                hypot.add(argument.to_number(cx)?);
            }
            hypot.result()
        }
    }))
}

#[inline(always)]
fn extreme(
    cx: &mut Cx,
    frame: &CallFrame,
    of_nothing: f64,
    of_integers: impl Fn(i32, i32) -> i32,
    of_numbers: impl Fn(f64, f64) -> f64,
) -> JsResult<Value> {
    if let [a, b] = *frame.arguments()
        && let (Some(a), Some(b)) = (a.as_int32(), b.as_int32())
    {
        return Ok(of_integers(a, b).into());
    }
    let mut result = of_nothing;
    for argument in frame.arguments() {
        result = of_numbers(result, argument.to_number(cx)?);
    }
    Ok(result.into())
}

#[native_abi]
fn max(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    extreme(cx, frame, f64::NEG_INFINITY, i32::max, math::max)
}

#[native_abi]
pub(super) fn min(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    extreme(cx, frame, f64::INFINITY, i32::min, math::min)
}

#[native_abi]
fn random(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    let global = cx.global();
    let mut state = global.random_state().get();
    let result = state.next_f64();
    global.random_state().set(state);
    Ok(Value::double(result))
}
