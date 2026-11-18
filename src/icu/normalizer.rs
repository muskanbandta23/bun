//! UAX #15.

use crate::error::Status;
use crate::ffi::{call, fill, length, open};
use crate::{Buffer, Result};

bun_opaque::opaque_ffi! { struct UNormalizer2; }

icu_functions! {
    fn unorm2_getNFCInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFDInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFKCInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_getNFKDInstance(status: &mut Status) -> *mut UNormalizer2;
    fn unorm2_isNormalized(normalizer: *const UNormalizer2, text: *const u16, length: i32, status: &mut Status) -> i8;
    fn unorm2_normalize(
        normalizer: *const UNormalizer2,
        text: *const u16,
        length: i32,
        out: *mut u16,
        capacity: i32,
        status: &mut Status,
    ) -> i32;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Form {
    /// A constant of the process that ICU owns.
    fn normalizer(self) -> Result<*const UNormalizer2> {
        let instance = match self {
            Form::Nfc => unorm2_getNFCInstance,
            Form::Nfd => unorm2_getNFDInstance,
            Form::Nfkc => unorm2_getNFKCInstance,
            Form::Nfkd => unorm2_getNFKDInstance,
        };
        // SAFETY: it takes nothing but the status.
        open(|status| unsafe { instance(status) }).map(|it| it.as_ptr().cast_const())
    }

    pub fn is_normalized(self, text: &[u16]) -> Result<bool> {
        let (normalizer, length) = (self.normalizer()?, length(text)?);
        // SAFETY: `text` has `length` units.
        call(
            |status| unsafe { unorm2_isNormalized(normalizer, text.as_ptr(), length, status) } != 0,
        )
    }

    pub fn normalize(self, text: &[u16], out: &mut Buffer<u16>) -> Result<()> {
        let (normalizer, length) = (self.normalizer()?, length(text)?);
        // SAFETY: `text` has `length` units, and `out` has room for `capacity`.
        fill(out, |out, capacity, status| unsafe {
            unorm2_normalize(normalizer, text.as_ptr(), length, out, capacity, status)
        })
    }
}
