//! What a program has run, where it spends its time and what it keeps in memory.

mod coverage;

pub use coverage::{BlockRange, CodeRange, CodeRangeKind, Coverage, Gaps, cover};
