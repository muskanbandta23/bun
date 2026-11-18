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
