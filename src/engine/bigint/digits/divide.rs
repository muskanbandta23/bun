use super::{add_assign_same_length, join, shift_bits_left, shift_bits_right};
use crate::{Error, Scratch, Work};

/// `high < divisor`. Returns the quotient and the remainder.
#[inline(always)]
fn divide_pair(high: u64, low: u64, divisor: u64) -> (u64, u64) {
    let quotient = (join(low, high) / u128::from(divisor)) as u64;
    (quotient, low.wrapping_sub(quotient.wrapping_mul(divisor)))
}

/// `out = a / divisor`. `out` is as long as `a`, or one digit shorter if the quotient fits. Returns the remainder.
pub(crate) fn divide_by_digit(out: &mut [u64], a: &[u64], divisor: u64) -> u64 {
    let (a, top) = a.split_at(out.len());
    let mut remainder = top.first().copied().unwrap_or(0);
    for (out, &a) in out.iter_mut().zip(a).rev() {
        (*out, remainder) = divide_pair(remainder, a, divisor);
    }
    remainder
}

/// `a /= divisor`. Returns the remainder.
pub(crate) fn divide_assign_by_digit(a: &mut [u64], divisor: u64) -> u64 {
    let mut remainder = 0;
    for a in a.iter_mut().rev() {
        (*a, remainder) = divide_pair(remainder, *a, divisor);
    }
    remainder
}

pub(crate) fn remainder_by_digit(a: &[u64], divisor: u64) -> u64 {
    a.iter()
        .rev()
        .fold(0, |remainder, &a| divide_pair(remainder, a, divisor).1)
}

/// Knuth, The Art of Computer Programming, 4.3.1, algorithm D. `b` is trimmed, has two digits or more and is no
/// longer than `a`. `quotient` has `a.len() - b.len() + 1` digits, or one less if the quotient fits, and
/// `remainder` is as long as `b`.
pub(crate) fn divide(
    mut quotient: Option<&mut [u64]>,
    remainder: Option<&mut [u64]>,
    a: &[u64],
    b: &[u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    let length = b.len();
    let shift = b[length - 1].leading_zeros();
    let mut scratch = Scratch::<u64, 128>::new();
    let (divisor, rest) = scratch
        .filled(0, length + a.len() + 1)?
        .split_at_mut(length);
    shift_bits_left(divisor, b, shift);
    rest[a.len()] = shift_bits_left(&mut rest[..a.len()], a, shift);
    let (second, first) = (divisor[length - 2], divisor[length - 1]);

    for row in (0..=a.len() - length).rev() {
        work.spend(length)?;
        let (window, above) = rest[row..].split_at_mut(length);
        let top = above[0];
        let mut digit = u64::MAX;
        if top != first {
            let mut left;
            (digit, left) = divide_pair(top, window[length - 1], first);
            while u128::from(digit) * u128::from(second) > join(window[length - 2], left) {
                digit -= 1;
                let Some(sum) = left.checked_add(first) else {
                    break;
                };
                left = sum;
            }
        }
        let mut carry = 0;
        for (rest, &divisor) in window.iter_mut().zip(&*divisor) {
            let (low, high) = divisor.carrying_mul(digit, carry);
            let borrow;
            (*rest, borrow) = rest.overflowing_sub(low);
            carry = high + u64::from(borrow);
        }
        if carry > top {
            add_assign_same_length(window, divisor);
            digit -= 1;
        }
        if let Some(out) = quotient.as_deref_mut().and_then(|out| out.get_mut(row)) {
            *out = digit;
        }
    }
    if let Some(remainder) = remainder {
        shift_bits_right(remainder, &rest[..length], shift);
    }
    Ok(())
}
