use core::cmp::Ordering;

use crate::{BigIntRef, Error, Fill, Filled, Outcome, Poll, Work, digits};

/// What has been worked out about the divisors seen last, for when they come again.
#[derive(Default)]
pub struct DivisorCache {}

/// Rounds towards zero.
#[inline]
pub fn divide<'a>(
    x: BigIntRef<'a>,
    y: BigIntRef<'a>,
    _: &'a DivisorCache,
) -> Result<Outcome<impl Fill + 'a>, Error> {
    let is_negative = x.is_negative != y.is_negative;
    match (x.digits, y.digits) {
        (_, []) => return Err(Error::DivisionByZero),
        (&[a], &[b]) => return Ok(Outcome::small(is_negative, u128::from(a / b))),
        (_, [1]) if !y.is_negative => return Ok(Outcome::First),
        _ => {}
    }
    let Some(extra) = x.digits.len().checked_sub(y.digits.len()) else {
        return Ok(Outcome::ZERO);
    };
    if let (Some(a), Some(b)) = (digits::to_u128(x.digits), digits::to_u128(y.digits)) {
        return Ok(Outcome::small(is_negative, a / b));
    }
    let top_is_zero = digits::compare_same_length(&x.digits[extra..], y.digits) == Ordering::Less;
    let length = extra + 1 - usize::from(top_is_zero);
    if length == 0 {
        return Ok(Outcome::ZERO);
    }
    Ok(Outcome::large(Quotient {
        length,
        a: x.digits,
        b: y.digits,
        is_negative,
    }))
}

struct Quotient<'a> {
    length: usize,
    a: &'a [u64],
    b: &'a [u64],
    is_negative: bool,
}

impl Fill for Quotient<'_> {
    #[inline(always)]
    fn digit_count(&self) -> usize {
        self.length
    }

    #[inline]
    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.length];
        match *self.b {
            [divisor] => {
                digits::divide_by_digit(out, self.a, divisor);
            }
            _ => digits::divide(Some(out), None, self.a, self.b, &mut Work::new(poll))?,
        }
        Ok(Filled::whole(self.is_negative, out))
    }
}

/// Has the sign of `x`.
#[inline]
pub fn remainder<'a>(
    x: BigIntRef<'a>,
    y: BigIntRef<'a>,
    _: &'a DivisorCache,
) -> Result<Outcome<impl Fill + 'a>, Error> {
    match (x.digits, y.digits) {
        (_, []) => return Err(Error::DivisionByZero),
        (&[a], &[b]) if a >= b => return Ok(Outcome::small(x.is_negative, u128::from(a % b))),
        _ => {}
    }
    match digits::compare(x.digits, y.digits) {
        Ordering::Less => return Ok(Outcome::First),
        Ordering::Equal => return Ok(Outcome::ZERO),
        Ordering::Greater => {}
    }
    if let [divisor] = *y.digits {
        let magnitude = digits::remainder_by_digit(x.digits, divisor);
        return Ok(Outcome::small(x.is_negative, u128::from(magnitude)));
    }
    if let (Some(a), Some(b)) = (digits::to_u128(x.digits), digits::to_u128(y.digits)) {
        return Ok(Outcome::small(x.is_negative, a % b));
    }
    Ok(Outcome::large(Remainder {
        a: x.digits,
        b: y.digits,
        is_negative: x.is_negative,
    }))
}

struct Remainder<'a> {
    a: &'a [u64],
    b: &'a [u64],
    is_negative: bool,
}

impl Fill for Remainder<'_> {
    #[inline(always)]
    fn digit_count(&self) -> usize {
        self.b.len()
    }

    #[inline]
    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.b.len()];
        digits::divide(None, Some(out), self.a, self.b, &mut Work::new(poll))?;
        Ok(Filled::trimmed(self.is_negative, out))
    }
}
