//! The operators on values of any type.

use bun_engine_bigint as bigint;
use bun_engine_number::{math, to_int32};

use crate::{Cx, Gc, JsBigInt, JsResult, JsString, Numeric, PreferredType, Value};

#[inline(always)]
fn binary(
    cx: &mut Cx,
    lhs: Value,
    rhs: Value,
    on_numbers: impl FnOnce(f64, f64) -> Value,
    on_bigints: impl FnOnce(&Cx, &JsBigInt, &JsBigInt) -> JsResult<Gc<JsBigInt>>,
    mixed: &'static str,
) -> JsResult<Value> {
    match (lhs.to_numeric(cx)?, rhs.to_numeric(cx)?) {
        (Numeric::Number(a), Numeric::Number(b)) => Ok(on_numbers(a, b)),
        (Numeric::BigInt(a), Numeric::BigInt(b)) => on_bigints(cx, &a, &b).map(Into::into),
        _ => Err(cx.throw_type_error(mixed)),
    }
}

macro_rules! binary_operators {
    ($($name:ident, $what:literal, |$a:ident, $b:ident| $on_numbers:expr, |$cx:ident, $x:ident, $y:ident| $on_bigints:expr;)*) => {$(
        pub fn $name(cx: &mut Cx, lhs: Value, rhs: Value) -> JsResult<Value> {
            binary(
                cx,
                lhs,
                rhs,
                |$a, $b| Value::from($on_numbers),
                |$cx, a, b| {
                    let ($x, $y) = (a.value(), b.value());
                    JsBigInt::from_outcome($cx, a, b, $on_bigints)
                },
                concat!("Invalid mix of BigInt and other type in ", $what, "."),
            )
        }
    )*};
}

binary_operators! {
    add_numeric, "addition", |a, b| a + b, |cx, x, y| Ok(bigint::add(x, y));
    subtract, "subtraction", |a, b| a - b, |cx, x, y| Ok(bigint::subtract(x, y));
    multiply, "multiplication", |a, b| a * b, |cx, x, y| bigint::multiply(x, y);
    divide, "division", |a, b| a / b, |cx, x, y| bigint::divide(x, y, cx.divisor_cache());
    remainder, "remainder", |a, b| math::remainder(a, b), |cx, x, y| bigint::remainder(x, y, cx.divisor_cache());
    exponentiate, "exponentiation", |a, b| math::pow(a, b), |cx, x, y| bigint::exponentiate(x, y);
    bit_and, "bitwise 'and' operation", |a, b| to_int32(a) & to_int32(b), |cx, x, y| Ok(bigint::bitwise_and(x, y));
    bit_or, "bitwise 'or' operation", |a, b| to_int32(a) | to_int32(b), |cx, x, y| Ok(bigint::bitwise_or(x, y));
    bit_xor, "bitwise 'xor' operation", |a, b| to_int32(a) ^ to_int32(b), |cx, x, y| Ok(bigint::bitwise_xor(x, y));
    shift_left, "left shift operation", |a, b| to_int32(a).wrapping_shl(to_int32(b) as u32),
        |cx, x, y| bigint::left_shift(x, y);
    shift_right, "signed right shift operation", |a, b| to_int32(a).wrapping_shr(to_int32(b) as u32),
        |cx, x, y| bigint::signed_right_shift(x, y);
}

pub fn shift_right_unsigned(cx: &mut Cx, lhs: Value, rhs: Value) -> JsResult<u32> {
    match (lhs.to_numeric(cx)?, rhs.to_numeric(cx)?) {
        (Numeric::Number(a), Numeric::Number(b)) => {
            Ok((to_int32(a) as u32).wrapping_shr(to_int32(b) as u32))
        }
        _ => Err(cx.throw_type_error("BigInt does not support >>> operator")),
    }
}

#[inline]
pub fn add(cx: &mut Cx, lhs: Value, rhs: Value) -> JsResult<Value> {
    if let (Some(a), Some(b)) = (lhs.as_string(), rhs.as_string()) {
        return JsString::concat(cx, a, b).map(Into::into);
    }
    let a = lhs.to_primitive(cx, PreferredType::None)?;
    let b = rhs.to_primitive(cx, PreferredType::None)?;
    if a.is_string() || b.is_string() {
        let (a, b) = (a.primitive_to_string(cx)?, b.primitive_to_string(cx)?);
        return JsString::concat(cx, a, b).map(Into::into);
    }
    add_numeric(cx, a, b)
}

macro_rules! unary_operators {
    ($($name:ident, |$a:ident| $on_number:expr, $on_bigint:path;)*) => {$(
        pub fn $name(cx: &mut Cx, value: Value) -> JsResult<Value> {
            match value.to_numeric(cx)? {
                Numeric::Number($a) => Ok(Value::from($on_number)),
                Numeric::BigInt(a) => JsBigInt::from_outcome(cx, &a, &a, Ok($on_bigint(a.value()))).map(Into::into),
            }
        }
    )*};
}

unary_operators! {
    negate, |a| -a, bigint::unary_minus;
    bit_not, |a| !to_int32(a), bigint::bitwise_not;
    increment, |a| a + 1.0, bigint::increment;
    decrement, |a| a - 1.0, bigint::decrement;
}
