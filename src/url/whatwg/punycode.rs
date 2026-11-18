//! RFC 3492.

const BASE: u32 = 36;
const T_MIN: u32 = 1;
const T_MAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 0x80;

/// The longest label that DNS has, which bounds the cost of inserting.
pub(crate) const MAX_LABEL_LENGTH: usize = 63;

fn adapt(delta: u32, count: u32, is_first: bool) -> u32 {
    let mut delta = delta / if is_first { DAMP } else { 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > (BASE - T_MIN) * T_MAX / 2 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
}

fn digit_value(digit: u8) -> Option<u32> {
    match digit {
        b'a'..=b'z' => Some(u32::from(digit - b'a')),
        b'0'..=b'9' => Some(u32::from(digit - b'0') + 26),
        _ => None,
    }
}

/// `encoded` is lowercase ASCII and fits in `storage`. No character takes less than a byte.
pub(crate) fn decode<'a>(
    encoded: &[u8],
    storage: &'a mut [char; MAX_LABEL_LENGTH],
) -> Option<&'a [char]> {
    let (basic, extended) = match bun_highway::last_index_of_char(encoded, b'-') {
        Some(delimiter @ 1..) => (&encoded[..delimiter], &encoded[delimiter + 1..]),
        _ => (&[][..], encoded),
    };
    for (character, &byte) in storage.iter_mut().zip(basic) {
        *character = char::from(byte);
    }
    let mut length = basic.len();
    let (mut n, mut i, mut bias) = (INITIAL_N, 0u32, INITIAL_BIAS);
    let mut digits = extended.iter().peekable();
    while digits.peek().is_some() {
        let previous = i;
        let (mut weight, mut k) = (1u32, BASE);
        loop {
            let digit = digit_value(*digits.next()?)?;
            i = i.checked_add(digit.checked_mul(weight)?)?;
            let threshold = k.saturating_sub(bias).clamp(T_MIN, T_MAX);
            if digit < threshold {
                break;
            }
            weight = weight.checked_mul(BASE - threshold)?;
            k += BASE;
        }
        let count = length as u32 + 1;
        bias = adapt(i - previous, count, previous == 0);
        n = n.checked_add(i / count)?;
        i %= count;
        storage.copy_within(i as usize..length, i as usize + 1);
        storage[i as usize] = char::from_u32(n)?;
        length += 1;
        i += 1;
    }
    Some(&storage[..length])
}
