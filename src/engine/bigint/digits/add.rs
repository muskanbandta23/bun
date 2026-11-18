/// `out = a + b`, all of one length. Returns the carry.
#[inline]
pub(crate) fn add_same_length(out: &mut [u64], a: &[u64], b: &[u64]) -> bool {
    debug_assert!(out.len() == a.len() && a.len() == b.len());
    let mut carry = false;
    for ((out, &a), &b) in out.iter_mut().zip(a).zip(b) {
        (*out, carry) = a.carrying_add(b, carry);
    }
    carry
}

/// `out = a - b`, all of one length. Returns the borrow.
#[inline]
pub(crate) fn subtract_same_length(out: &mut [u64], a: &[u64], b: &[u64]) -> bool {
    debug_assert!(out.len() == a.len() && a.len() == b.len());
    let mut borrow = false;
    for ((out, &a), &b) in out.iter_mut().zip(a).zip(b) {
        (*out, borrow) = a.borrowing_sub(b, borrow);
    }
    borrow
}

/// `a += b`, both of one length. Returns the carry.
#[inline]
pub(crate) fn add_assign_same_length(a: &mut [u64], b: &[u64]) -> bool {
    debug_assert!(a.len() == b.len());
    let mut carry = false;
    for (a, &b) in a.iter_mut().zip(b) {
        (*a, carry) = a.carrying_add(b, carry);
    }
    carry
}

/// `out = a + carry`, both of one length. Returns the carry.
#[inline]
pub(crate) fn copy_with_carry(out: &mut [u64], a: &[u64], mut carry: bool) -> bool {
    let mut done = 0;
    while carry && done < a.len() {
        (out[done], carry) = a[done].overflowing_add(1);
        done += 1;
    }
    out[done..].copy_from_slice(&a[done..]);
    carry
}

/// `out = a - borrow`, both of one length. Returns the borrow.
#[inline]
pub(crate) fn copy_with_borrow(out: &mut [u64], a: &[u64], mut borrow: bool) -> bool {
    let mut done = 0;
    while borrow && done < a.len() {
        (out[done], borrow) = a[done].overflowing_sub(1);
        done += 1;
    }
    out[done..].copy_from_slice(&a[done..]);
    borrow
}

/// `out = a + b`. `out` is as long as `a`, and `b` is no longer. Returns the carry.
#[inline]
pub(crate) fn add(out: &mut [u64], a: &[u64], b: &[u64]) -> bool {
    let (out_low, out_high) = out.split_at_mut(b.len());
    let (a_low, a_high) = a.split_at(b.len());
    copy_with_carry(out_high, a_high, add_same_length(out_low, a_low, b))
}

/// `out = a - b`. `out` is as long as `a`, and `b` is no longer. Returns the borrow.
#[inline]
pub(crate) fn subtract(out: &mut [u64], a: &[u64], b: &[u64]) -> bool {
    let (out_low, out_high) = out.split_at_mut(b.len());
    let (a_low, a_high) = a.split_at(b.len());
    copy_with_borrow(out_high, a_high, subtract_same_length(out_low, a_low, b))
}

/// `a += 1`. Returns the carry.
#[inline]
pub(crate) fn increment(a: &mut [u64]) -> bool {
    a.iter_mut().all(|digit| {
        *digit = digit.wrapping_add(1);
        *digit == 0
    })
}
