//! Conversions between numbers and text, and the numeric semantics of the language.

#![forbid(unsafe_code)]

mod convert;
mod decimal_scanner;
mod decimal_to_double;
mod digit_words;
mod double;
mod expansion;
mod integer_text;
mod json;
mod literal;
mod number_text;
mod numeric_index;
mod powers_of_ten;
mod radix_scanner;
mod shortest;
mod shortest_text;
mod string_to_number;

pub use convert::{exact_int32, to_int32, to_uint32};
pub use decimal_scanner::parse_decimal_prefix;
pub use integer_text::{decimal_length, write_i32, write_i64, write_u32, write_u64};
pub use json::{JsonNumber, JsonNumberError, parse_json_number};
pub use literal::{decimal_literal_value, radix_literal_value};
pub use number_text::{NUMBER_TEXT_CAPACITY, NumberText};
pub use numeric_index::canonical_numeric_index;
pub use shortest_text::write_f64;
pub use string_to_number::{parse_float, string_to_number};
