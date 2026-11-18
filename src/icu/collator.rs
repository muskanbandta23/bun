//! UTS #10 with the tailorings of CLDR.

use core::cmp::Ordering;
use core::ffi::{CStr, c_char};

use crate::enumeration::UEnumeration;
use crate::error::Status;
use crate::ffi::{call, length, open};
use crate::locale::listed;
use crate::{Enumeration, Result};

bun_opaque::opaque_ffi! { struct UCollator; }

icu_functions! {
    fn ucol_open(locale: *const c_char, status: &mut Status) -> *mut UCollator;
    fn ucol_close(collator: *mut UCollator);
    fn ucol_setAttribute(collator: *mut UCollator, attribute: Attribute, value: Value, status: &mut Status);
    fn ucol_getAttribute(collator: *const UCollator, attribute: Attribute, status: &mut Status) -> i32;
    fn ucol_getRules(collator: *const UCollator, length: &mut i32) -> *const u16;
    fn ucol_strcoll(collator: *const UCollator, a: *const u16, a_length: i32, b: *const u16, b_length: i32) -> i32;
    fn ucol_strcollUTF8(collator: *const UCollator, a: *const u8, a_length: i32, b: *const u8, b_length: i32, status: &mut Status) -> i32;
    fn ucol_countAvailable() -> i32;
    fn ucol_getAvailable(index: i32) -> *const c_char;
    fn ucol_getKeywordValues(key: *const c_char, status: &mut Status) -> *mut UEnumeration;
    fn ucol_getKeywordValuesForLocale(key: *const c_char, locale: *const c_char, commonly_used: i8, status: &mut Status) -> *mut UEnumeration;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Attribute {
    BackwardSecondary = 0,
    AlternateHandling = 1,
    CaseFirst = 2,
    CaseLevel = 3,
    Normalization = 4,
    Strength = 5,
    Numeric = 7,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Value {
    Primary = 0,
    Secondary = 1,
    Tertiary = 2,
    Off = 16,
    On = 17,
    Shifted = 20,
    NonIgnorable = 21,
    LowerFirst = 24,
    UpperFirst = 25,
}

icu_handle! {
    pub struct Collator(UCollator), ucol_close
}

impl Collator {
    pub fn open(locale: &CStr) -> Result<Collator> {
        // SAFETY: `locale` is terminated.
        open(|status| unsafe { ucol_open(locale.as_ptr(), status) }).map(Collator)
    }

    pub fn set(&mut self, attribute: Attribute, value: Value) -> Result<()> {
        // SAFETY: the collator is open, and ICU refuses a value that is not one of the attribute's.
        call(|status| unsafe { ucol_setAttribute(self.0.as_ptr(), attribute, value, status) })
    }

    pub fn has(&self, attribute: Attribute, value: Value) -> Result<bool> {
        // SAFETY: the collator is open.
        call(|status| unsafe { ucol_getAttribute(self.0.as_ptr(), attribute, status) })
            .map(|actual| actual == value as i32)
    }

    /// Whether rules change the root order of CLDR.
    pub fn is_tailored(&self) -> bool {
        let mut length = 0;
        // SAFETY: the collator is open.
        unsafe { ucol_getRules(self.0.as_ptr(), &mut length) };
        length != 0
    }

    pub fn compare(&self, a: &[u16], b: &[u16]) -> Result<Ordering> {
        let (a_length, b_length) = (length(a)?, length(b)?);
        // SAFETY: the collator is open, and the strings have these lengths.
        let order =
            unsafe { ucol_strcoll(self.0.as_ptr(), a.as_ptr(), a_length, b.as_ptr(), b_length) };
        Ok(order.cmp(&0))
    }

    /// An ill-formed sequence counts as U+FFFD.
    pub fn compare_utf8(&self, a: &[u8], b: &[u8]) -> Result<Ordering> {
        let (a_length, b_length) = (length(a)?, length(b)?);
        // SAFETY: the collator is open, and the strings have these lengths.
        call(|status| unsafe {
            ucol_strcollUTF8(
                self.0.as_ptr(),
                a.as_ptr(),
                a_length,
                b.as_ptr(),
                b_length,
                status,
            )
        })
        .map(|order| order.cmp(&0))
    }
}

pub fn available_locales() -> impl Iterator<Item = &'static CStr> {
    // SAFETY: the names belong to an array that ICU builds once and keeps.
    unsafe { listed(ucol_countAvailable(), ucol_getAvailable) }
}

/// The values of the `collation` keyword that any locale has.
pub fn all_types() -> Result<Enumeration> {
    // SAFETY: the key is terminated.
    Enumeration::open(|status| unsafe { ucol_getKeywordValues(c"collation".as_ptr(), status) })
}

/// The values of the `collation` keyword that `locale` has, its default first.
pub fn types_for_locale(locale: &CStr) -> Result<Enumeration> {
    // SAFETY: both strings are terminated.
    Enumeration::open(|status| unsafe {
        ucol_getKeywordValuesForLocale(c"collation".as_ptr(), locale.as_ptr(), 0, status)
    })
}
