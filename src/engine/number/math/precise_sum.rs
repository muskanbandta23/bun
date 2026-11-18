use crate::double::{
    FRACTION_BITS, FRACTION_MASK, IMPLICIT_BIT, MAX_BIASED_EXPONENT, SIGN_BIT, biased_exponent,
    with_sign,
};

const CHUNK_BITS: u32 = 32;
/// 64 for the exponents, one for the upper part of a term and three for carries.
const CHUNKS: usize = 68;
/// A term adds at most 2^52 to a chunk.
const TERMS_BETWEEN_CARRIES: u32 = (1 << 11) - 2;

/// The exact sum of up to 2^53 numbers, rounded once (Math.sumPrecise). After the small superaccumulator of Neal, "Fast
/// exact summation using small and large superaccumulators".
pub struct PreciseSum {
    /// Chunk `i` counts in units of `2^(32 * i - 1075)`.
    chunks: [i64; CHUNKS],
    /// Bit `i` is set if chunk `i` or `i + 1` has been added to.
    used: u64,
    terms_until_carry: u32,
    /// The bits that all terms have.
    common_bits: u64,
    /// The sum of the terms that are not finite.
    special: f64,
}

impl Default for PreciseSum {
    fn default() -> PreciseSum {
        PreciseSum {
            chunks: [0; CHUNKS],
            used: 0,
            terms_until_carry: TERMS_BETWEEN_CARRIES,
            common_bits: u64::MAX,
            special: 0.0,
        }
    }
}

impl PreciseSum {
    #[inline]
    pub fn add(&mut self, term: f64) {
        let bits = term.to_bits();
        self.common_bits &= bits;
        let (significand, exponent) = match biased_exponent(bits) {
            0 => (bits & FRACTION_MASK, 1),
            MAX_BIASED_EXPONENT => {
                core::hint::cold_path();
                self.special += term;
                return;
            }
            biased => (bits & FRACTION_MASK | IMPLICIT_BIT, biased),
        };
        let sign = bits as i64 >> 63;
        let signed = (significand as i64 ^ sign) - sign;
        let (index, shift) = (exponent / CHUNK_BITS, exponent % CHUNK_BITS);
        self.used |= 1 << index;
        self.chunks[index as usize] += i64::from((signed << shift) as u32);
        self.chunks[index as usize + 1] += signed >> (CHUNK_BITS - shift);
        self.terms_until_carry -= 1;
        if self.terms_until_carry == 0 {
            self.carry();
        }
    }

    #[cold]
    fn carry(&mut self) {
        let lowest = self.used.trailing_zeros() as usize;
        let mut carry = 0;
        for chunk in &mut self.chunks[lowest..CHUNKS - 1] {
            let sum = *chunk + carry;
            *chunk = i64::from(sum as i32);
            carry = (sum - *chunk) >> CHUNK_BITS;
        }
        self.chunks[CHUNKS - 1] += carry;
        self.used = u64::MAX << lowest;
        self.terms_until_carry = TERMS_BETWEEN_CARRIES;
    }

    #[inline]
    pub fn result(mut self) -> f64 {
        self.normalize_and_round()
    }

    fn normalize_and_round(&mut self) -> f64 {
        if self.special != 0.0 {
            return self.special;
        }
        let lowest = self.used.trailing_zeros() as usize;
        let end = CHUNKS - self.used.leading_zeros() as usize;
        let Some(chunks) = self.chunks.get_mut(lowest..end) else {
            return with_sign(0.0, self.common_bits & SIGN_BIT != 0);
        };

        // To two's complement digits, then to the magnitude.
        let mut carry = 0;
        for chunk in chunks.iter_mut() {
            let sum = *chunk + carry;
            *chunk = i64::from(sum as u32);
            carry = sum >> CHUNK_BITS;
        }
        let is_negative = carry < 0;
        if is_negative {
            let mut carry = 1;
            for chunk in chunks.iter_mut() {
                let sum = i64::from(!(*chunk as u32)) + carry;
                *chunk = i64::from(sum as u32);
                carry = sum >> CHUNK_BITS;
            }
        }

        let Some(top) = chunks.iter().rposition(|&chunk| chunk != 0) else {
            return with_sign(0.0, self.common_bits & SIGN_BIT != 0);
        };
        let (rest, leading) = chunks[..=top].split_at(top.saturating_sub(2));
        let is_inexact = rest.iter().any(|&chunk| chunk != 0);
        let leading = leading
            .iter()
            .rev()
            .fold(0, |bits, &chunk| bits << CHUNK_BITS | chunk as u128);
        let position = (lowest + rest.len()) as u32 * CHUNK_BITS;
        with_sign(
            round(leading | u128::from(is_inexact), position),
            is_negative,
        )
    }
}

/// `bits * 2^(position - 1075)`
fn round(bits: u128, position: u32) -> f64 {
    let top_bit = position + bits.ilog2();
    // Where the last place of the result is, and by the choice of units its biased exponent.
    let biased = top_bit.saturating_sub(FRACTION_BITS).max(1);
    if biased >= MAX_BIASED_EXPONENT {
        return f64::INFINITY;
    }
    let significand = match biased.checked_sub(position) {
        None | Some(0) => (bits << (position - biased)) as u64,
        Some(shift) => {
            let just_below_half = (1 << (shift - 1)) - 1;
            ((bits + just_below_half + (bits >> shift & 1)) >> shift) as u64
        }
    };
    f64::from_bits((u64::from(biased - 1) << FRACTION_BITS) + significand)
}
