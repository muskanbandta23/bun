//! Arbitrary-precision integer arithmetic on digit slices.
//!
//! An operation takes two steps, so that the caller allocates the result. The function that is named after it does
//! what takes no storage and returns an [`Outcome`]: one of the operands, a [`SmallBigInt`], or a [`Fill`] that
//! says how many digits it needs and writes them.

#![forbid(unsafe_code)]
#![feature(maybe_uninit_fill)]

mod add;
mod bitwise;
mod compare;
mod convert;
mod digits;
mod divide;
mod error;
mod format;
mod hash;
mod multiply;
mod outcome;
mod owned;
mod parse;
mod power;
mod radix;
mod shift;
mod truncate;
mod value;
mod work;

pub use add::{add, bitwise_not, decrement, increment, subtract, unary_minus};
pub use bitwise::{bitwise_and, bitwise_or, bitwise_xor};
pub use convert::from_number;
pub use divide::{DivisorCache, divide, remainder};
pub use error::{Error, ErrorKind};
pub use multiply::multiply;
pub use outcome::{Fill, Filled, New, Outcome};
pub use owned::BigInt;
pub use parse::{LiteralRadix, from_literal, from_string};
pub use power::exponentiate;
pub use shift::{left_shift, signed_right_shift};
pub use truncate::{as_int_n, as_uint_n};
pub use value::{BigIntRef, SmallBigInt};
pub use work::Poll;

use work::{Scratch, Work};

pub const MAX_BITS: usize = 1 << 30;
/// The most digits a value has.
pub const MAX_LENGTH: usize = MAX_BITS / 64;
