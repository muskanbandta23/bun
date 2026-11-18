//! Locale identifiers in the syntax of ICU (`sr_Latn_RS@calendar=gregorian`).

use core::ffi::{CStr, c_char};

use crate::enumeration::UEnumeration;
use crate::error::Status;
use crate::ffi::{call, filled};
use crate::{Buffer, CText, Enumeration, Error, Result};

icu_functions! {
    fn uloc_forLanguageTag(tag: *const c_char, out: *mut u8, capacity: i32, parsed: &mut i32, status: &mut Status) -> i32;
    fn uloc_toLanguageTag(locale: *const c_char, out: *mut u8, capacity: i32, strict: i8, status: &mut Status) -> i32;
    fn uloc_openKeywords(locale: *const c_char, status: &mut Status) -> *mut UEnumeration;
}

/// Fails unless all of `tag` is a BCP 47 language tag.
pub fn for_language_tag(tag: &CStr) -> Result<CText> {
    let mut parsed = 0;
    // SAFETY: `tag` is terminated, and `out` has room for `capacity`.
    let locale = filled(|out, capacity, status| unsafe {
        uloc_forLanguageTag(tag.as_ptr(), out, capacity, &mut parsed, status)
    })?;
    if usize::try_from(parsed) != Ok(tag.count_bytes()) {
        return Err(Error::ILLEGAL_ARGUMENT);
    }
    CText::from_buffer(locale)
}

pub fn to_language_tag(locale: &CStr) -> Result<Buffer<u8>> {
    // SAFETY: `locale` is terminated, and `out` has room for `capacity`.
    filled(|out, capacity, status| unsafe {
        uloc_toLanguageTag(locale.as_ptr(), out, capacity, 0, status)
    })
}

/// `None` if the locale has no keywords.
pub fn keywords(locale: &CStr) -> Result<Option<Enumeration>> {
    // SAFETY: `locale` is terminated.
    let keywords = call(|status| unsafe { uloc_openKeywords(locale.as_ptr(), status) })?;
    if keywords.is_null() {
        return Ok(None);
    }
    Enumeration::open(|_| keywords).map(Some)
}
