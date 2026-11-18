mod digit;
mod narrow;
mod recursive;

pub(crate) use digit::*;

use super::{
    MAX_WIDTH, compare, decrement, increment, low_zero_count, multiply, shift_bits_left,
    shift_bits_right, trim,
};
use crate::{Error, Scratch, Work};
use narrow::DivisorTop;
use recursive::Division;

/// `b` is trimmed and no longer than `a`. `quotient` has `a.len() - b.len() + 1` digits, or one less if the quotient
/// fits, and `remainder` is as long as `b`. Either can be empty if it is not wanted.
pub(crate) fn divide(
    quotient: &mut [u64],
    mut remainder: &mut [u64],
    mut a: &[u64],
    mut b: &[u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    if b[0] == 0 {
        let zeros = low_zero_count(b);
        if let Some(low) = remainder.split_off_mut(..zeros) {
            low.copy_from_slice(&a[..zeros]);
        }
        (a, b) = (&a[zeros..], &b[zeros..]);
    }
    if let [divisor] = *b {
        let left = if quotient.is_empty() {
            remainder_by_digit(a, divisor)
        } else {
            divide_by_digit(quotient, a, divisor)
        };
        remainder.fill(left);
        return work.spend(4 * a.len());
    }
    if b.len() <= MAX_WIDTH {
        narrow::divide(quotient, remainder, a, b);
        return work.spend(2 * MAX_WIDTH * a.len());
    }
    if remainder.is_empty() {
        return divide_for_quotient(quotient, a, b, work);
    }
    divide_wide(quotient, remainder, a, b, work)
}

/// Fills `divisor` and `dividend`, which is one digit longer than `a`, so that the top bit of the divisor is set.
fn normalize(dividend: &mut [u64], divisor: &mut [u64], a: &[u64], b: &[u64]) -> u32 {
    let shift = b[b.len() - 1].leading_zeros();
    shift_bits_left(divisor, b, shift);
    dividend[a.len()] = shift_bits_left(&mut dividend[..a.len()], a, shift);
    shift
}

fn divide_wide(
    quotient: &mut [u64],
    remainder: &mut [u64],
    a: &[u64],
    b: &[u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    let length = a.len() - b.len() + 1;
    let mut scratch = Scratch::<u64, 384>::new();
    let scratch = scratch.filled(0, 2 * b.len() + a.len() + 1 + length)?;
    let (divisor, scratch) = scratch.split_at_mut(b.len());
    let (product, scratch) = scratch.split_at_mut(b.len());
    let (dividend, whole_quotient) = scratch.split_at_mut(a.len() + 1);
    let shift = normalize(dividend, divisor, a, b);
    let top = DivisorTop::of(divisor);
    let fits = dividend[a.len()] == 0 && dividend[a.len() - 1] < divisor[b.len() - 1];
    let needed = length - usize::from(fits);
    Division { top, product, work }.divide(
        &mut whole_quotient[..needed],
        &mut dividend[..needed + b.len()],
        divisor,
    )?;
    quotient.copy_from_slice(&whole_quotient[..quotient.len()]);
    shift_bits_right(remainder, &dividend[..b.len()], shift);
    Ok(())
}

/// Works out a digit below the last one of the quotient, and looks at the low digits of the operands only if that
/// leaves a doubt.
fn divide_for_quotient(
    quotient: &mut [u64],
    a: &[u64],
    b: &[u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    const MAX_ERROR: u64 = 64;
    let length = a.len() - b.len() + 2;
    let unused = b.len().saturating_sub(length + 2);
    let (a_high, b_high) = (&a[unused..], &b[unused..]);
    let mut scratch = Scratch::<u64, 384>::new();
    let scratch = scratch.filled(0, 2 * b_high.len() + a_high.len() + 2 + length)?;
    let (divisor, scratch) = scratch.split_at_mut(b_high.len());
    let (product, scratch) = scratch.split_at_mut(b_high.len());
    let (dividend, estimate) = scratch.split_at_mut(a_high.len() + 2);
    normalize(&mut dividend[1..], divisor, a_high, b_high);
    let top = DivisorTop::of(divisor);
    Division { top, product, work }.divide_roughly(estimate, dividend, divisor)?;

    let (fraction, whole) = (estimate[0], &mut estimate[1..]);
    if fraction.wrapping_add(MAX_ERROR) < 2 * MAX_ERROR {
        let wraps = fraction >= MAX_ERROR && increment(whole);
        let mut scratch = Scratch::<u64, 128>::new();
        let product = scratch.filled(0, a.len() + 1)?;
        multiply(product, whole, b, work)?;
        if wraps || compare(trim(product), trim(a)).is_gt() {
            decrement(whole);
        }
    }
    quotient.copy_from_slice(&whole[..quotient.len()]);
    Ok(())
}
