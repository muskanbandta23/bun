mod common;

use bun_engine_bigint::*;
use common::*;

const PRIME: u128 = (1 << 61) - 1;

/// By plain linear code.
fn residue(x: &BigInt) -> u128 {
    let digits = x.as_ref().digits().iter().rev();
    digits.fold(0, |left, &digit| (left << 64 | u128::from(digit)) % PRIME)
}

struct Operands(u64);

impl Operands {
    fn digit(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mixed = (self.0 ^ self.0 >> 30).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed ^ mixed >> 29
    }

    /// Dense, all ones, or mostly zeros.
    fn with_length(&mut self, length: usize) -> BigInt {
        let kind = self.digit() % 3;
        let mut digits: Vec<u64> = (0..length)
            .map(|_| match (kind, self.digit()) {
                (0, digit) => digit,
                (1, _) => u64::MAX,
                (_, digit) => u64::from(digit % 8 == 0),
            })
            .collect();
        digits[length - 1] |= 1;
        BigInt::copy_of(BigIntRef::new(false, &digits)).unwrap()
    }
}

/// Around the lengths at which another algorithm takes over.
const LENGTHS: [usize; 22] = [
    1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 33, 64, 129, 130, 131, 200, 395, 520, 581, 1406, 1407, 3000,
];

#[test]
fn products() {
    let mut operands = Operands(1);
    for long in LENGTHS {
        let x = operands.with_length(long);
        let square = residue(&x) * residue(&x) % PRIME;
        assert_eq!(residue(&apply("*", &x, &x)), square, "{long}");
        for short in LENGTHS.into_iter().filter(|&short| short <= long) {
            let y = operands.with_length(short);
            let product = residue(&x) * residue(&y) % PRIME;
            assert_eq!(residue(&apply("*", &x, &y)), product, "{long} {short}");
        }
    }
}

#[test]
fn quotients_and_remainders() {
    let mut operands = Operands(2);
    let one = big("1");
    for divisor_length in LENGTHS {
        for quotient_length in LENGTHS {
            let divisor = operands.with_length(divisor_length);
            let quotient = operands.with_length(quotient_length);
            let product = apply("*", &quotient, &divisor);
            let halved = apply(">>", &divisor, &one);
            for left in [big("0"), halved, apply("-", &divisor, &one)] {
                let dividend = apply("+", &product, &left);
                let results = (
                    apply("/", &dividend, &divisor),
                    apply("%", &dividend, &divisor),
                );
                let is_right = results.0 == quotient && results.1 == left;
                assert!(is_right, "{divisor_length} {quotient_length}");
            }
        }
    }
}
