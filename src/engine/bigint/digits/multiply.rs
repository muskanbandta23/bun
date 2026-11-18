use crate::{Error, Work};

/// `a = a * factor + carry`. Returns the digit above.
#[inline]
pub(crate) fn multiply_assign_digit(a: &mut [u64], factor: u64, mut carry: u64) -> u64 {
    for a in a {
        (*a, carry) = a.carrying_mul(factor, carry);
    }
    carry
}

/// `out = a * b`. Neither is empty. `out` is as long as both together, or one digit shorter if the product fits.
pub(crate) fn multiply(
    out: &mut [u64],
    a: &[u64],
    b: &[u64],
    work: &mut Work<'_>,
) -> Result<(), Error> {
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    debug_assert!(a.len() + b.len() - out.len() <= 1);
    out.fill(0);
    for (row, &factor) in b.iter().enumerate() {
        work.spend(a.len())?;
        let (sum, above) = out[row..].split_at_mut(a.len());
        let mut carry = 0;
        for (sum, &a) in sum.iter_mut().zip(a) {
            (*sum, carry) = a.carrying_mul_add(factor, *sum, carry);
        }
        if let Some(above) = above.first_mut() {
            *above = carry;
        }
    }
    Ok(())
}
