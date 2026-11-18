mod cookie;
mod cursor;
mod free_form;
mod iso;

use bun_engine_text::{Text, with_text};

pub use cookie::parse_cookie_date;
use cursor::Spaces;

use crate::time_value::time_clip;

/// In milliseconds since the epoch, and not clipped.
#[derive(Clone, Copy)]
pub(crate) enum Parsed {
    Utc(i64),
    /// The text names no zone.
    Local(i64),
}

/// The Date Time String Format of ECMA-262, and failing that what scripts have long been able to pass to `Date.parse`.
pub(crate) fn parse_date(text: Text<'_>) -> Option<Parsed> {
    with_text!(text, |units| free_form::parse(units, Spaces::Unicode))
}

/// `Date.parse` for UTF-8 from outside of scripts. White space is ASCII, and a text that names no zone is in UTC.
pub fn parse_date_as_utc(bytes: &[u8]) -> f64 {
    match free_form::parse(bytes, Spaces::Ascii) {
        Some(Parsed::Utc(time) | Parsed::Local(time)) => time_clip(time as f64),
        None => f64::NAN,
    }
}
