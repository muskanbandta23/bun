//! The tables of CLDR as ICU stores them.

use core::ffi::{CStr, c_char};
use core::ptr;

use crate::Result;
use crate::error::Status;
use crate::ffi::{adopt, borrowed, call};

bun_opaque::opaque_ffi! { struct UResourceBundle; }

icu_functions! {
    fn ures_open(package: *const c_char, locale: *const c_char, status: &mut Status) -> *mut UResourceBundle;
    fn ures_openDirect(package: *const c_char, name: *const c_char, status: &mut Status) -> *mut UResourceBundle;
    fn ures_close(bundle: *mut UResourceBundle);
    fn ures_getByKey(
        bundle: *const UResourceBundle,
        key: *const c_char,
        fill_in: *mut UResourceBundle,
        status: &mut Status,
    ) -> *mut UResourceBundle;
    fn ures_getStringByKey(bundle: *const UResourceBundle, key: *const c_char, length: &mut i32, status: &mut Status) -> *const u16;
}

icu_handle! {
    /// A table, or an item of one. It does not need the bundle that it came from.
    pub struct ResourceBundle(UResourceBundle), ures_close
}

// SAFETY: a lookup only reads the bundle, and ICU counts the users of the data under a lock.
unsafe impl Sync for ResourceBundle {}

impl ResourceBundle {
    /// The top table falls back to the parents of the locale for a key that it lacks.
    pub fn for_locale(locale: &CStr) -> Result<ResourceBundle> {
        adopt(
            // SAFETY: `locale` is terminated, and no package means the data of ICU.
            |status| unsafe { ures_open(ptr::null(), locale.as_ptr(), status) },
            ResourceBundle,
        )
    }

    /// Data that belongs to no locale: `metadata`, `supplementalData`.
    pub fn named(name: &CStr) -> Result<ResourceBundle> {
        adopt(
            // SAFETY: `name` is terminated, and no package means the data of ICU.
            |status| unsafe { ures_openDirect(ptr::null(), name.as_ptr(), status) },
            ResourceBundle,
        )
    }

    pub fn get(&self, key: &CStr) -> Result<ResourceBundle> {
        adopt(
            // SAFETY: the bundle is open, `key` is terminated, and without an object to fill in the result is new.
            |status| unsafe {
                ures_getByKey(self.0.as_ptr(), key.as_ptr(), ptr::null_mut(), status)
            },
            ResourceBundle,
        )
    }

    pub fn get_string(&self, key: &CStr) -> Result<&[u16]> {
        let mut length = 0;
        // SAFETY: the bundle is open and `key` is terminated.
        let string = call(|status| unsafe {
            ures_getStringByKey(self.0.as_ptr(), key.as_ptr(), &mut length, status)
        })?;
        // SAFETY: part of data that stays loaded.
        Ok(unsafe { borrowed(string, length) })
    }
}
