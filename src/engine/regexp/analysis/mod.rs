//! What can be known about a pattern before it meets a subject.

mod backtracking;
mod next;

pub(crate) use backtracking::decide_backtracking;
pub(crate) use next::Next;
