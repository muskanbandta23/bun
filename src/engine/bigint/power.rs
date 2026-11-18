use crate::{BigIntRef, Error, Fill, Filled, MAX_BITS, Outcome, Poll, Scratch, Work, digits};

#[inline]
pub fn exponentiate<'a>(
    base: BigIntRef<'a>,
    exponent: BigIntRef<'a>,
) -> Result<Outcome<impl Fill + 'a>, Error> {
    if exponent.is_negative {
        return Err(Error::NegativeExponent);
    }
    let [low, rest @ ..] = exponent.digits else {
        return Ok(Outcome::small(false, 1));
    };
    let is_negative = base.is_negative && low & 1 != 0;
    match base.digits {
        [] => return Ok(Outcome::First),
        [1] => return Ok(Outcome::small(is_negative, 1)),
        _ => {}
    }
    if !rest.is_empty() || *low >= MAX_BITS as u64 {
        return Err(Error::TooBig);
    }
    let exponent = *low as u32;
    if exponent == 1 {
        return Ok(Outcome::First);
    }
    if let [digit] = *base.digits
        && let Some(power) = u128::from(digit).checked_pow(exponent)
    {
        return Ok(Outcome::small(is_negative, power));
    }
    let bound = UpperBound::of_power(base.digits, exponent);
    if (bound.bit_length, bound.significand) >= (MAX_BITS as u64 + 1, 1 << 63 | 1 << 32) {
        return Err(Error::TooBig);
    }
    Ok(Outcome::large(Power {
        length: (bound.bit_length as usize).div_ceil(64),
        base: base.digits,
        exponent,
        is_negative,
    }))
}

/// `significand * 2^(bit_length - 64)`, with the top bit of the significand set.
#[derive(Clone, Copy)]
struct UpperBound {
    significand: u64,
    bit_length: u64,
}

impl UpperBound {
    fn new(significand: u64, bit_length: u64, is_exact: bool) -> UpperBound {
        match significand.checked_add(u64::from(!is_exact)) {
            Some(significand) => UpperBound {
                significand,
                bit_length,
            },
            None => UpperBound {
                significand: 1 << 63,
                bit_length: bit_length + 1,
            },
        }
    }

    fn times(self, other: UpperBound) -> UpperBound {
        let product = u128::from(self.significand) * u128::from(other.significand);
        let shift = product.leading_zeros();
        let product = product << shift;
        UpperBound::new(
            (product >> 64) as u64,
            self.bit_length + other.bit_length - u64::from(shift),
            product as u64 == 0,
        )
    }

    /// The roundings add up to a factor below `1 + exponent * 2^-61`.
    fn of_power(base: &[u64], exponent: u32) -> UpperBound {
        let base = UpperBound::new(
            digits::top_bits(base),
            digits::bit_length(base) as u64,
            !digits::has_bits_below_top(base),
        );
        let mut power = base;
        for bit in (0..exponent.ilog2()).rev() {
            power = power.times(power);
            if exponent >> bit & 1 != 0 {
                power = power.times(base);
            }
        }
        power
    }
}

struct Power<'a> {
    length: usize,
    base: &'a [u64],
    exponent: u32,
    is_negative: bool,
}

impl Fill for Power<'_> {
    #[inline(always)]
    fn digit_count(&self) -> usize {
        self.length
    }

    fn fill(self, out: &mut [u64], poll: Poll<'_>) -> Result<Filled, Error> {
        let out = &mut out[..self.length];
        let zero_digits = digits::low_zero_count(self.base);
        let base = &self.base[zero_digits..];
        let zero_bits = base[0].trailing_zeros();
        let shift = (zero_digits * 64 + zero_bits as usize) * self.exponent as usize;

        let capacity = self.length - shift / 64 + 1;
        let mut scratch = Scratch::<u64, 128>::new();
        let (odd, buffers) = scratch
            .filled(0, base.len() + 2 * capacity)?
            .split_at_mut(base.len());
        digits::shift_bits_right(odd, base, zero_bits);
        let power = raise(
            digits::trim(odd),
            self.exponent,
            buffers.split_at_mut(capacity),
            &mut Work::new(poll),
        )?;
        digits::shift_left(out, power, shift);
        Filled::whole_or_one_less(self.is_negative, out)
    }
}

/// Each buffer has room for one digit more than the result.
fn raise<'a>(
    base: &[u64],
    exponent: u32,
    (mut power, mut next): (&'a mut [u64], &'a mut [u64]),
    work: &mut Work<'_>,
) -> Result<&'a [u64], Error> {
    let mut length = base.len();
    power[..length].copy_from_slice(base);
    for bit in (0..exponent.ilog2()).rev() {
        let (factor, product) = (&power[..length], &mut next[..2 * length]);
        digits::multiply(product, factor, factor, work)?;
        length = digits::trim(product).len();
        core::mem::swap(&mut power, &mut next);
        if exponent >> bit & 1 != 0 {
            let product = &mut next[..length + base.len()];
            digits::multiply(product, &power[..length], base, work)?;
            length = digits::trim(product).len();
            core::mem::swap(&mut power, &mut next);
        }
    }
    Ok(&power[..length])
}
