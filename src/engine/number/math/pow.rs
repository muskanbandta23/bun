/// Compiled code that multiplies in line has to stop at the same exponent to give the same results.
pub const MAX_EXPONENT_BY_SQUARING: i32 = 1000;

/// Number::exponentiate of ECMA-262.
pub fn pow(base: f64, exponent: f64) -> f64 {
    let magnitude = base.abs();
    if exponent.is_nan() || (magnitude == 1.0 && exponent.is_infinite()) {
        return f64::NAN;
    }
    if exponent.abs() == 0.5 {
        let is_own_root = magnitude == 0.0 || magnitude == f64::INFINITY;
        let root = if is_own_root { magnitude } else { base.sqrt() };
        return if exponent > 0.0 { root } else { 1.0 / root };
    }
    let integer = exponent as i32;
    if f64::from(integer) == exponent && (0..=MAX_EXPONENT_BY_SQUARING).contains(&integer) {
        return by_squaring(base, integer);
    }
    base.powf(exponent)
}

#[inline]
fn by_squaring(mut base: f64, mut exponent: i32) -> f64 {
    let mut result = 1.0;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result *= base;
        }
        base *= base;
        exponent >>= 1;
    }
    result
}
