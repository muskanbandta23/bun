use bun_engine_text::Text;

use crate::{NumberText, string_to_number};

/// CanonicalNumericIndexString of ECMA-262.
#[inline]
pub fn canonical_numeric_index(text: Text<'_>) -> Option<f64> {
    match u8::try_from(text.get(0)?) {
        Ok(b'0'..=b'9' | b'-' | b'I' | b'N') => by_round_trip(text),
        _ => None,
    }
}

fn by_round_trip(text: Text<'_>) -> Option<f64> {
    /// A sign, "0.", five zeros and seventeen digits.
    const MAX_LENGTH: usize = 25;
    if text.len() > MAX_LENGTH {
        return None;
    }
    if text.eq_ascii("-0") {
        return Some(-0.0);
    }
    let number = string_to_number(text);
    (NumberText::from(number).text() == text).then_some(number)
}
