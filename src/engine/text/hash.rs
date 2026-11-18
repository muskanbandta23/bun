//! rapidhash, cut to the 24 bits that a `StringImpl` keeps. Equal strings hash alike whichever width stores them.
//! `src/codegen/create_hash_table` computes the same function for the static property tables.

const SECRET: [u64; 3] = [
    0x2d35_8dcc_aa6c_78a5,
    0x8bb8_4b93_962e_acc9,
    0x4b33_a62e_d433_d4a3,
];
const HASH_MASK: u32 = (1 << 24) - 1;
/// Zero means "not computed yet".
const HASH_IN_PLACE_OF_ZERO: u32 = 1 << 23;

#[inline(always)]
const fn multiply(a: u64, b: u64) -> (u64, u64) {
    let product = a as u128 * b as u128;
    (product as u64, (product >> 64) as u64)
}

#[inline(always)]
const fn mix(a: u64, b: u64) -> u64 {
    let (low, high) = multiply(a, b);
    low ^ high
}

macro_rules! rapidhash {
    ($length:expr, $read8:expr, $read32:expr, $read64:expr, $data:expr) => {{
        let data = $data;
        let length: usize = $length;
        let mut seed = mix(SECRET[0], SECRET[1]) ^ length as u64;
        let (a, b) = if length <= 16 {
            if length >= 4 {
                let delta = if length >= 8 { 4 } else { 0 };
                (
                    ($read32(data, 0) << 32) | $read32(data, length - 4),
                    ($read32(data, delta) << 32) | $read32(data, length - 4 - delta),
                )
            } else if length > 0 {
                (
                    ($read8(data, 0) << 56)
                        | ($read8(data, length >> 1) << 32)
                        | $read8(data, length - 1),
                    0,
                )
            } else {
                (0, 0)
            }
        } else {
            let mut left = length;
            let mut at = 0;
            if left > 48 {
                let (mut second, mut third) = (seed, seed);
                while left >= 48 {
                    seed = mix($read64(data, at) ^ SECRET[0], $read64(data, at + 8) ^ seed);
                    second = mix(
                        $read64(data, at + 16) ^ SECRET[1],
                        $read64(data, at + 24) ^ second,
                    );
                    third = mix(
                        $read64(data, at + 32) ^ SECRET[2],
                        $read64(data, at + 40) ^ third,
                    );
                    at += 48;
                    left -= 48;
                }
                seed ^= second ^ third;
            }
            if left > 16 {
                seed = mix(
                    $read64(data, at) ^ SECRET[2],
                    $read64(data, at + 8) ^ seed ^ SECRET[1],
                );
                if left > 32 {
                    seed = mix(
                        $read64(data, at + 16) ^ SECRET[2],
                        $read64(data, at + 24) ^ seed,
                    );
                }
            }
            ($read64(data, length - 16), $read64(data, length - 8))
        };
        let (low, high) = multiply(a ^ SECRET[1], b ^ seed);
        let hash = mix(low ^ SECRET[0] ^ length as u64, high ^ SECRET[1]) as u32 & HASH_MASK;
        if hash == 0 {
            HASH_IN_PLACE_OF_ZERO
        } else {
            hash
        }
    }};
}

#[inline(always)]
const fn byte(data: &[u8], at: usize) -> u64 {
    data[at] as u64
}

#[inline(always)]
const fn bytes32(data: &[u8], at: usize) -> u64 {
    match data.split_at(at).1.first_chunk::<4>() {
        Some(chunk) => u32::from_le_bytes(*chunk) as u64,
        None => unreachable!(),
    }
}

#[inline(always)]
const fn bytes64(data: &[u8], at: usize) -> u64 {
    match data.split_at(at).1.first_chunk::<8>() {
        Some(chunk) => u64::from_le_bytes(*chunk),
        None => unreachable!(),
    }
}

#[inline(always)]
fn narrowed<const N: usize>(data: &[u16], at: usize) -> u64 {
    data[at..at + N]
        .iter()
        .rev()
        .fold(0, |word, &unit| word << 8 | u64::from(unit))
}

#[inline]
pub const fn hash_latin1(characters: &[u8]) -> u32 {
    rapidhash!(characters.len(), byte, bytes32, bytes64, characters)
}

pub fn hash_utf16(units: &[u16]) -> u32 {
    if units.iter().fold(0, |all, &unit| all | unit) > 0xff {
        return hash_latin1(bun_pod::slice_as_bytes(units));
    }
    rapidhash!(
        units.len(),
        narrowed::<1>,
        narrowed::<4>,
        narrowed::<8>,
        units
    )
}
