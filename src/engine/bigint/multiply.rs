use crate::{BigIntRef, Error, Fill, Filled, MAX_LENGTH, Outcome, Poll, Work, digits};

/// The most digits that the operands of a small product have together.
const MAX_SMALL_LENGTH: usize = 2 * digits::MAX_WIDTH;

#[inline(always)]
pub fn multiply<'a>(x: BigIntRef<'a>, y: BigIntRef<'a>) -> Result<Outcome<impl Fill + 'a>, Error> {
    let is_negative = x.is_negative != y.is_negative;
    if let (&[a], &[b]) = (x.digits, y.digits) {
        return Ok(Outcome::small(is_negative, u128::from(a) * u128::from(b)));
    }
    let (long, short) = digits::long_and_short(x.digits, y.digits);
    let short_top = match *short {
        [_, .., top] => top,
        [] if x.is_zero() => return Ok(Outcome::First),
        [] => return Ok(Outcome::Second),
        [factor] => {
            if let [low, high] = *long {
                let (low, carry) = low.carrying_mul(factor, 0);
                if let (middle, 0) = high.carrying_mul(factor, carry) {
                    return Ok(Outcome::small(is_negative, digits::join(low, middle)));
                }
            }
            match (x.digits, y.digits) {
                (_, [1]) if !y.is_negative => return Ok(Outcome::First),
                ([1], _) if !x.is_negative => return Ok(Outcome::Second),
                _ => factor,
            }
        }
    };
    let mut length = long.len() + short.len();
    if length <= MAX_SMALL_LENGTH {
        let top_bits = (long[long.len() - 1] | 1).ilog2() + (short_top | 1).ilog2();
        length -= usize::from(top_bits < 63);
    } else if length - 1 > MAX_LENGTH {
        return Err(Error::TooBig);
    }
    Ok(Outcome::large(Product {
        length,
        long,
        short,
        is_negative,
    }))
}

struct Product<'a> {
    /// That of a small product leaves out a top digit that is sure to be zero.
    length: usize,
    long: &'a [u64],
    short: &'a [u64],
    is_negative: bool,
}

impl Fill for Product<'_> {
    #[inline(always)]
    fn digit_count(&self) -> usize {
        self.length
    }

    #[inline(always)]
    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.length];
        let below_top = self.long.len() + self.short.len() - 1;
        if below_top < MAX_SMALL_LENGTH {
            let top = digits::multiply_narrow(out, self.long, self.short);
            if let Some(out) = out.get_mut(below_top) {
                *out = top;
            }
            return Ok(Filled {
                length: (below_top + usize::from(top != 0)) as u32,
                is_negative: self.is_negative,
            });
        }
        digits::multiply(out, self.long, self.short, &mut Work::new(poll))?;
        Filled::whole_or_one_less(self.is_negative, out)
    }
}
