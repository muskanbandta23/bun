//! Every decimal digit of a binary number.

use crate::digit_words::{ZEROS, eight_digits, split_sixteen_digits, store};
use crate::powers_of_ten::floor_log10_of_power_of_two;

pub(crate) const BLOCK_DIGITS: usize = 19;
const BLOCK: u64 = 10_000_000_000_000_000_000;
/// 2^1025 has 309 digits.
const MAX_WHOLE_BLOCKS: usize = 17;
/// At most 768 binary places are left once a value is scaled past the zeros after its decimal point.
const MAX_FRACTION_LIMBS: usize = 12;

/// Yields blocks of 19 digits, the most significant first, until only zeros are left.
pub(crate) struct Expansion {
    /// In base 10^19, the least significant first.
    whole: [u64; MAX_WHOLE_BLOCKS],
    whole_count: usize,
    /// In base 2^64, the least significant first, with the point after the last limb of the array.
    fraction: [u64; MAX_FRACTION_LIMBS],
    /// The limbs below this one are zero.
    fraction_start: usize,
    /// How many digits of the blocks precede the decimal point.
    point: i32,
}

impl Expansion {
    /// `significand * 2^exponent`, from 2^-1075 to below 2^1025, with a significand below 2^54.
    pub(crate) fn new(significand: u64, exponent: i32) -> Expansion {
        let mut expansion = Expansion {
            whole: [0; _],
            whole_count: 0,
            fraction: [0; _],
            fraction_start: MAX_FRACTION_LIMBS,
            point: 0,
        };
        if exponent >= 0 {
            expansion.set_integer(significand, exponent as usize);
        } else {
            let places = exponent.unsigned_abs();
            let whole = significand.checked_shr(places).unwrap_or(0);
            if whole == 0 {
                expansion.set_fraction(significand, places);
            } else {
                expansion.whole[0] = whole;
                expansion.whole_count = 1;
                expansion.point = BLOCK_DIGITS as i32;
                expansion.fraction_start -= 1;
                expansion.fraction[expansion.fraction_start] = significand << (64 - places);
            }
        }
        expansion
    }

    fn set_integer(&mut self, significand: u64, shift: usize) {
        let mut limbs = [0u64; 17];
        let shifted = u128::from(significand) << (shift % 64);
        limbs[shift / 64] = shifted as u64;
        limbs[shift / 64 + 1] = (shifted >> 64) as u64;
        let mut count = shift / 64 + 2;
        while count > 0 {
            if limbs[count - 1] == 0 {
                count -= 1;
                continue;
            }
            let mut remainder = 0;
            for limb in limbs[..count].iter_mut().rev() {
                (*limb, remainder) = divide_by_block(remainder, *limb);
            }
            self.whole[self.whole_count] = remainder;
            self.whole_count += 1;
        }
        self.point = (self.whole_count * BLOCK_DIGITS) as i32;
    }

    /// `significand / 2^places`, which is below one.
    fn set_fraction(&mut self, significand: u64, places: u32) {
        const LARGEST_POWER: u32 = 27;
        let bits = 64 - significand.leading_zeros();
        let zeros = (-1 - floor_log10_of_power_of_two(bits as i32 - places as i32)).max(0) as u32;
        self.point = -(zeros as i32);

        // Times 10^zeros: the fives multiply the significand and the twos move the point.
        let places = places - zeros;
        let limbs = places.div_ceil(64);
        self.fraction_start -= limbs as usize;
        let shifted = u128::from(significand) << (limbs * 64 - places);
        let fraction = &mut self.fraction[self.fraction_start..];
        fraction[0] = shifted as u64;
        let mut count = 1;
        let mut carry = (shifted >> 64) as u64;
        let mut fives = zeros;
        loop {
            if carry != 0 {
                fraction[count] = carry;
                count += 1;
            }
            if fives == 0 {
                break;
            }
            let step = fives.min(LARGEST_POWER);
            carry = multiply(&mut fraction[..count], 5u64.pow(step));
            fives -= step;
        }
    }

    pub(crate) fn digits(mut self) -> Digits {
        let mut point = self.point;
        let block = self.next().unwrap_or_default();
        let at = block.iter().take_while(|&&digit| digit == b'0').count();
        point -= at as i32;
        Digits {
            expansion: self,
            block,
            at,
            point,
        }
    }
}

impl Iterator for Expansion {
    /// Characters. The first block has a nonzero digit.
    type Item = [u8; BLOCK_DIGITS];

    fn next(&mut self) -> Option<[u8; BLOCK_DIGITS]> {
        if self.whole_count > 0 {
            self.whole_count -= 1;
            return Some(block_characters(self.whole[self.whole_count]));
        }
        while *self.fraction.get(self.fraction_start)? == 0 {
            self.fraction_start += 1;
        }
        let block = multiply(&mut self.fraction[self.fraction_start..], BLOCK);
        Some(block_characters(block))
    }
}

/// Yields digit values from the first nonzero one. Zeros may follow the last nonzero one.
pub(crate) struct Digits {
    expansion: Expansion,
    block: [u8; BLOCK_DIGITS],
    at: usize,
    point: i32,
}

impl Digits {
    /// How many digits precede the decimal point.
    pub(crate) fn point(&self) -> i32 {
        self.point
    }
}

impl Iterator for Digits {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        if self.at == BLOCK_DIGITS {
            self.block = self.expansion.next()?;
            self.at = 0;
        }
        self.at += 1;
        Some(self.block[self.at - 1] - b'0')
    }
}

/// Returns the limb that is carried out.
fn multiply(limbs: &mut [u64], factor: u64) -> u64 {
    let mut carry = 0;
    for limb in limbs {
        (*limb, carry) = limb.carrying_mul(factor, carry);
    }
    carry
}

/// Quotient and remainder of `high * 2^64 + low`, with `high` below the divisor. Möller and Granlund, "Improved
/// division by invariant integers".
#[inline]
fn divide_by_block(high: u64, low: u64) -> (u64, u64) {
    const RECIPROCAL: u64 = (u128::MAX / BLOCK as u128 - (1 << 64)) as u64;
    let estimate =
        u128::from(RECIPROCAL) * u128::from(high) + (u128::from(high) << 64 | u128::from(low));
    let mut quotient = ((estimate >> 64) as u64).wrapping_add(1);
    let mut remainder = low.wrapping_sub(quotient.wrapping_mul(BLOCK));
    if remainder > estimate as u64 {
        quotient = quotient.wrapping_sub(1);
        remainder = remainder.wrapping_add(BLOCK);
    }
    if remainder >= BLOCK {
        quotient += 1;
        remainder -= BLOCK;
    }
    (quotient, remainder)
}

fn block_characters(block: u64) -> [u8; BLOCK_DIGITS] {
    const SIXTEEN_DIGITS: u64 = 10_000_000_000_000_000;
    let mut characters = [0; BLOCK_DIGITS];
    let (middle, lower) = split_sixteen_digits(block % SIXTEEN_DIGITS);
    let upper = eight_digits((block / SIXTEEN_DIGITS) as u32);
    store(&mut characters, 0, upper >> 40 | ZEROS);
    store(&mut characters, 3, eight_digits(middle));
    store(&mut characters, 11, eight_digits(lower));
    characters
}
