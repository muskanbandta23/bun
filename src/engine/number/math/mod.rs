//! The functions of the Math object and the operators that are not single instructions.

mod hypot;
mod pow;
mod precise_sum;
mod remainder;

pub use hypot::{Hypot, hypot, hypot3};
pub use pow::{MAX_EXPONENT_BY_SQUARING, pow};
pub use precise_sum::PreciseSum;
pub use remainder::{power_of_two_divisor, remainder};

use core::f64::consts::FRAC_PI_4;

use crate::double::{EXPONENT_BIAS, FRACTION_BITS, FRACTION_MASK, SIGN_BIT, biased_exponent};
use crate::{float16_from_f64, float16_to_f64};

macro_rules! from_methods {
    ($($function:ident = $method:ident,)*) => {$(
        #[inline]
        pub fn $function(x: f64) -> f64 {
            x.$method()
        }
    )*};
}

from_methods! {
    abs = abs,
    acos = acos,
    acosh = acosh,
    asin = asin,
    asinh = asinh,
    atan = atan,
    cbrt = cbrt,
    ceil = ceil,
    cos = cos,
    cosh = cosh,
    exp = exp,
    expm1 = exp_m1,
    floor = floor,
    log = ln,
    log10 = log10,
    log1p = ln_1p,
    log2 = log2,
    sin = sin,
    sinh = sinh,
    sqrt = sqrt,
    tan = tan,
    tanh = tanh,
    trunc = trunc,
}

pub fn atan2(y: f64, x: f64) -> f64 {
    if x.is_infinite() && y.is_infinite() {
        let quarters = if x > 0.0 { 1.0 } else { 3.0 };
        return (quarters * FRAC_PI_4).copysign(y);
    }
    y.atan2(x)
}

/// `f64::atanh` is not the function of the C library, and is less accurate than this.
pub fn atanh(x: f64) -> f64 {
    let magnitude = x.abs();
    if magnitude < 1.0 / (1 << 28) as f64 {
        return x;
    }
    let twice = magnitude + magnitude;
    let ratio = if magnitude < 0.5 {
        twice + twice * magnitude / (1.0 - magnitude)
    } else {
        twice / (1.0 - magnitude)
    };
    (0.5 * ratio.ln_1p()).copysign(x)
}

#[inline]
pub fn round(x: f64) -> f64 {
    let ceiling = x.ceil();
    ceiling - if ceiling - 0.5 > x { 1.0 } else { 0.0 }
}

#[inline]
pub fn sign(x: f64) -> f64 {
    if x == 0.0 || x.is_nan() {
        return x;
    }
    1f64.copysign(x)
}

#[inline]
pub fn max(a: f64, b: f64) -> f64 {
    a.maximum(b)
}

#[inline]
pub fn min(a: f64, b: f64) -> f64 {
    a.minimum(b)
}

#[inline]
pub fn fround(x: f64) -> f64 {
    f64::from(x as f32)
}

#[inline]
pub fn f16round(x: f64) -> f64 {
    float16_to_f64(float16_from_f64(x))
}

/// A factor that has the same effect on every number as dividing by `divisor`.
pub fn exact_reciprocal(divisor: f64) -> Option<f64> {
    let bits = divisor.to_bits();
    let (biased, doubled_bias) = (biased_exponent(bits), 2 * EXPONENT_BIAS as u32);
    (bits & (SIGN_BIT | FRACTION_MASK) == 0 && (1..doubled_bias).contains(&biased))
        .then(|| f64::from_bits(u64::from(doubled_bias - biased) << FRACTION_BITS))
}
