//! Searches in the elements of arrays, from src/jsc/bindings/highway_elements.cpp.

unsafe extern "C" {
    fn highway_index_of_u64(elements: *const u64, len: usize, needle: u64) -> usize;
    fn highway_last_index_of_u64(elements: *const u64, len: usize, needle: u64) -> usize;
    fn highway_index_of_f64(elements: *const f64, len: usize, needle: f64) -> usize;
    fn highway_last_index_of_f64(elements: *const f64, len: usize, needle: f64) -> usize;
    fn highway_index_of_nan(elements: *const f64, len: usize) -> usize;
    fn highway_index_of_either_u64(elements: *const u64, len: usize, a: u64, b: u64) -> usize;
    fn highway_last_index_of_either_u64(elements: *const u64, len: usize, a: u64, b: u64) -> usize;
}

/// Below this a call costs more than it saves. It is at least the number of lanes of the widest vector.
const SHORT: usize = 8;

macro_rules! searches {
    ($($(#[$doc:meta])* $name:ident($type:ty $(, $needle:ident)*) = $kernel:ident, $scalar:ident(|$element:ident| $test:expr);)*) => {$(
        $(#[$doc])*
        #[inline(always)]
        pub fn $name(elements: &[$type] $(, $needle: $type)*) -> Option<usize> {
            if cfg!(miri) || elements.len() < SHORT {
                return elements.iter().$scalar(|&$element| $test);
            }
            // SAFETY: a readable range that fills a vector.
            let found = unsafe { $kernel(elements.as_ptr(), elements.len() $(, $needle)*) };
            (found < elements.len()).then_some(found)
        }
    )*};
}

searches! {
    index_of_u64(u64, needle) = highway_index_of_u64, position(|it| it == needle);
    last_index_of_u64(u64, needle) = highway_last_index_of_u64, rposition(|it| it == needle);
    /// The zeros are equal, and NaN is equal to nothing.
    index_of_f64(f64, needle) = highway_index_of_f64, position(|it| it == needle);
    /// As `index_of_f64`.
    last_index_of_f64(f64, needle) = highway_last_index_of_f64, rposition(|it| it == needle);
    index_of_nan(f64) = highway_index_of_nan, position(|it| it.is_nan());
    index_of_either_u64(u64, a, b) = highway_index_of_either_u64, position(|it| it == a || it == b);
    last_index_of_either_u64(u64, a, b) = highway_last_index_of_either_u64, rposition(|it| it == a || it == b);
}

/// Searches in memory that need not be aligned and that another thread may be writing. Each returns the index, or
/// `len` if there is none.
///
/// # Safety
/// `len` lanes at `elements` are readable, and they are at least [`MIN_LANE_BYTES`].
pub trait Lane: Copy {
    /// The first lane whose bits under the mask are `needle`.
    unsafe fn index_of_masked(elements: *const Self, len: usize, mask: Self, needle: Self)
    -> usize;

    /// The last one.
    unsafe fn last_index_of_masked(
        elements: *const Self,
        len: usize,
        mask: Self,
        needle: Self,
    ) -> usize;

    /// The first lane that is above `limit` without its highest bit.
    unsafe fn index_of_magnitude_above(elements: *const Self, len: usize, limit: Self) -> usize;
}

/// The size of the widest vector.
pub const MIN_LANE_BYTES: usize = 32;

macro_rules! lanes {
    ($($type:ty, $first:ident, $last:ident, $above:ident;)*) => {$(
        unsafe extern "C" {
            fn $first(elements: *const $type, len: usize, mask: $type, needle: $type) -> usize;
            fn $last(elements: *const $type, len: usize, mask: $type, needle: $type) -> usize;
            fn $above(elements: *const $type, len: usize, limit: $type) -> usize;
        }

        impl Lane for $type {
            #[inline(always)]
            unsafe fn index_of_masked(elements: *const $type, len: usize, mask: $type, needle: $type) -> usize {
                // SAFETY: the caller's.
                unsafe { $first(elements, len, mask, needle) }
            }

            #[inline(always)]
            unsafe fn last_index_of_masked(elements: *const $type, len: usize, mask: $type, needle: $type) -> usize {
                // SAFETY: the caller's.
                unsafe { $last(elements, len, mask, needle) }
            }

            #[inline(always)]
            unsafe fn index_of_magnitude_above(elements: *const $type, len: usize, limit: $type) -> usize {
                // SAFETY: the caller's.
                unsafe { $above(elements, len, limit) }
            }
        }
    )*};
}

lanes! {
    u8, highway_index_of_masked_8, highway_last_index_of_masked_8, highway_index_of_magnitude_above_8;
    u16, highway_index_of_masked_16, highway_last_index_of_masked_16, highway_index_of_magnitude_above_16;
    u32, highway_index_of_masked_32, highway_last_index_of_masked_32, highway_index_of_magnitude_above_32;
    u64, highway_index_of_masked_64, highway_last_index_of_masked_64, highway_index_of_magnitude_above_64;
}
