use crate::{BigIntRef, Error, Fill, Filled, MAX_LENGTH, Outcome, Poll, Work, digits};

#[inline]
pub fn multiply<'a>(x: BigIntRef<'a>, y: BigIntRef<'a>) -> Result<Outcome<impl Fill + 'a>, Error> {
    let is_negative = x.is_negative != y.is_negative;
    let (x_top, y_top) = match (x.digits, y.digits) {
        (&[a], &[b]) => return Ok(Outcome::small(is_negative, u128::from(a) * u128::from(b))),
        ([], _) => return Ok(Outcome::First),
        (_, []) => return Ok(Outcome::Second),
        (_, [1]) if !y.is_negative => return Ok(Outcome::First),
        ([1], _) if !x.is_negative => return Ok(Outcome::Second),
        (&[low, high], &[factor]) | (&[factor], &[low, high]) => {
            let (low, carry) = low.carrying_mul(factor, 0);
            let (middle, top) = high.carrying_mul(factor, carry);
            if top == 0 {
                return Ok(Outcome::small(is_negative, digits::join(low, middle)));
            }
            (high, factor)
        }
        (&[.., x_top], &[.., y_top]) => (x_top, y_top),
    };
    let length = x.digits.len() + y.digits.len();
    if length - 1 > MAX_LENGTH {
        return Err(Error::TooBig);
    }
    // The product is less than (x_top + 1) * (y_top + 1) in the place of the top digits.
    let top_is_zero = x_top.carrying_mul_add(y_top, x_top, y_top).1 == 0;
    Ok(Outcome::large(Product {
        length: length - usize::from(top_is_zero),
        a: x.digits,
        b: y.digits,
        is_negative,
    }))
}

struct Product<'a> {
    length: usize,
    a: &'a [u64],
    b: &'a [u64],
    is_negative: bool,
}

impl Fill for Product<'_> {
    #[inline(always)]
    fn digit_count(&self) -> usize {
        self.length
    }

    #[inline]
    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.length];
        digits::multiply(out, self.a, self.b, &mut Work::new(poll))?;
        Filled::whole_or_one_less(self.is_negative, out)
    }
}
