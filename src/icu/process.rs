//! What a process settles before it uses ICU.

use core::ffi::c_char;

use crate::argument::locale_id;
use crate::error::Status;
use crate::{CText, locale};

icu_functions! {
    fn uloc_setDefault(locale: *const c_char, status: &mut Status);
}

/// Makes ICU allocate as the rest of the process does. The library of macOS is shared with the system's frameworks, and
/// keeps their allocator.
///
/// # Safety
/// Nothing has called ICU yet, and nothing does until this returns.
pub unsafe fn use_default_allocator() {
    #[cfg(not(system_icucore))]
    {
        use bun_alloc::{default_alloc, out_of_memory};
        use core::ffi::c_void;

        type Allocate = extern "C" fn(*const c_void, usize) -> *mut c_void;
        type Reallocate = unsafe extern "C" fn(*const c_void, *mut c_void, usize) -> *mut c_void;
        type Free = unsafe extern "C" fn(*const c_void, *mut c_void);

        icu_functions! {
            fn u_setMemoryFunctions(context: *const c_void, allocate: Allocate, reallocate: Reallocate, free: Free, status: &mut Status);
        }

        // Comparing strings and looking for a boundary have no way to report a failure.
        fn allocated(block: *mut c_void) -> *mut c_void {
            if block.is_null() {
                out_of_memory();
            }
            block
        }

        extern "C" fn allocate(_: *const c_void, size: usize) -> *mut c_void {
            allocated(default_alloc::malloc(size))
        }

        unsafe extern "C" fn reallocate(
            _: *const c_void,
            block: *mut c_void,
            size: usize,
        ) -> *mut c_void {
            // SAFETY: each block that ICU holds came from these functions.
            allocated(unsafe { default_alloc::realloc(block, size) })
        }

        unsafe extern "C" fn free(_: *const c_void, block: *mut c_void) {
            // SAFETY: each block that ICU holds came from these functions.
            unsafe { default_alloc::free(block) }
        }

        // SAFETY: ICU holds no block of another allocator. It fails only if a function is missing.
        unsafe {
            u_setMemoryFunctions(
                core::ptr::null(),
                allocate,
                reallocate,
                free,
                &mut Status::default(),
            );
        }
    }
}

/// Takes `locale::default()` from the environment (`LC_ALL`, `LC_MESSAGES`, `LANG`) as far as that names a locale, and
/// makes it `en_US` if it names none or one whose data is not in this executable.
///
/// # Safety
/// No other thread calls ICU or changes the environment until this returns.
pub unsafe fn choose_default_locale() {
    let set = |locale| {
        // SAFETY: `locale` is terminated, or null for that of the environment. A failure leaves the default as it is.
        unsafe { uloc_setDefault(locale, &mut Status::default()) }
    };
    set(c"en_US".as_ptr());
    set(core::ptr::null());

    let found = locale::default();
    let usable = parents(found.to_bytes())
        .filter_map(|id| CText::new(id).ok())
        .find(|id| locale_id(id).is_ok());
    match usable {
        Some(id) if !data::was_removed(id.to_bytes()) => set(id.as_ptr()),
        None if found.is_empty() => {}
        _ => set(c"en_US".as_ptr()),
    }
}

/// `id`, then with one subtag after another taken off its end.
fn parents(id: &[u8]) -> impl Iterator<Item = &[u8]> {
    (1..=id.len())
        .rev()
        .filter(|&end| matches!(id.get(end), None | Some(b'_' | b'@')))
        .map(|end| &id[..end])
}

#[cfg(not(bundled_icu))]
mod data {
    pub(super) fn was_removed(_: &[u8]) -> bool {
        false
    }
}

#[cfg(bundled_icu)]
mod data {
    use core::cmp::Ordering;
    use core::ffi::CStr;

    /// What `--trim` leaves in the place of the items of a language. To open one ends the process.
    const TOMBSTONE: [u8; 16] = *b"BUNICUREMOVED\0\0\0";

    unsafe extern "C" {
        #[link_name = concat!(env!("BUN_ICU_DATA_NAME"), "_dat")]
        static PACKAGE: u8;
    }

    /// The format is in scripts/build/icu/package.ts.
    fn item(name: &[u8]) -> Option<*const u8> {
        let package = &raw const PACKAGE;
        // SAFETY: the package starts with the size of its header, which the table of the items follows.
        let contents = unsafe { package.add(u16::from_le_bytes(*package.cast()).into()) };
        // SAFETY: called for the count of the items, and for the two offsets of an item below it.
        let word =
            |index| u32::from_le_bytes(unsafe { *contents.cast::<[u8; 4]>().add(index) }) as usize;

        let (mut low, mut high) = (0, word(0));
        while low < high {
            let middle = low.midpoint(high);
            // SAFETY: the first offset of an item is that of its terminated name.
            let found = unsafe { CStr::from_ptr(contents.add(word(1 + 2 * middle)).cast()) };
            match name.cmp(found.to_bytes()) {
                Ordering::Less => high = middle,
                Ordering::Greater => low = middle + 1,
                // SAFETY: the second offset of an item is that of its data.
                Ordering::Equal => return Some(unsafe { contents.add(word(2 + 2 * middle)) }),
            }
        }
        None
    }

    pub(super) fn was_removed(locale: &[u8]) -> bool {
        const PREFIX: &[u8] = concat!(env!("BUN_ICU_DATA_NAME"), "l/").as_bytes();
        super::parents(locale)
            .find_map(|id| item(&[PREFIX, id, b".res"].concat()))
            // SAFETY: an item has at least 16 bytes.
            .is_some_and(|item| unsafe { item.cast::<[u8; 16]>().read() } == TOMBSTONE)
    }
}
