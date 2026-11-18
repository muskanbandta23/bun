//! Dates and time zones.

use bun_engine_date::{DateFields, current_time, parse_cookie_date, set_time_zone_override};
use bun_engine_text::{StringRef, Text};
use bun_icu::{Buffer, time_zone};

use super::abi::{RawText, or_abort, slice};

/// As `PlainGregorianDateTime` has them.
const _: () = {
    assert!(DateFields::YEAR.shift == 44 && DateFields::YEAR.width == 20);
    assert!(DateFields::MONTH.shift == 40 && DateFields::DAY.shift == 35);
    assert!(DateFields::WEEK_DAY.shift == 32 && DateFields::HOUR.shift == 27);
    assert!(DateFields::MINUTE.shift == 21 && DateFields::SECOND.shift == 15);
};

#[unsafe(no_mangle)]
extern "C" fn engine_date_current_time() -> f64 {
    current_time()
}

#[unsafe(no_mangle)]
unsafe extern "C" fn engine_date_parse_cookie_date(characters: *const u8, length: usize) -> f64 {
    // SAFETY: a span.
    parse_cookie_date(unsafe { slice(characters, length) }).map_or(f64::NAN, |time| time as f64)
}

#[unsafe(no_mangle)]
extern "C" fn engine_date_set_time_zone_override(name: RawText<'_>) -> bool {
    set_time_zone_override(name.text())
}

#[unsafe(no_mangle)]
extern "C" fn engine_date_canonical_time_zone(name: RawText<'_>) -> StringRef {
    let name: Buffer<u16> = name.text().code_units().collect();
    let canonical = time_zone::canonical_id(&name).unwrap_or_default();
    or_abort(StringRef::try_narrowed(Text::Utf16(&canonical)))
}
