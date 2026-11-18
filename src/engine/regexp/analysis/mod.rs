//! What can be known about a pattern before it meets a subject.

mod backtracking;
mod frequency;
mod next;
mod prefix;
mod scan;

pub(crate) use backtracking::decide_backtracking;
pub(crate) use frequency::Frequencies;
pub(crate) use next::Next;
pub(crate) use prefix::Prefixes;
pub(crate) use scan::plan_scan;
