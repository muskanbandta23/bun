/// Repeats a block for each index of a range that is known at compile time and ends at 8 or below, or at 16 if `wide`.
macro_rules! unroll {
    ($index:ident in $start:expr, $end:expr => $body:block) => {
        unroll!(@ $index, $start, $end, $body, 8: 0 1 2 3 4 5 6 7)
    };
    (wide $index:ident in $start:expr, $end:expr => $body:block) => {
        unroll!(@ $index, $start, $end, $body, 16: 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15)
    };
    (@ $index:ident, $start:expr, $end:expr, $body:block, $limit:literal: $($value:literal)*) => {
        debug_assert!($end <= $limit);
        $(
            #[allow(unused_comparisons, clippy::int_plus_one)]
            if $start <= $value && $value < $end {
                let $index: usize = $value;
                $body
            }
        )*
    };
}

pub(crate) use unroll;
