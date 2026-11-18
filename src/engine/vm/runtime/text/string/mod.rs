//! The methods of `String.prototype`, once their arguments have been converted.

mod convert;
mod join;
mod joiner;
mod recursion;
mod repeat;
mod replace;
mod search;
mod slice;
mod splice;
mod split;

pub use joiner::{JoinableNumber, StringJoiner};
pub use recursion::StringRecursionGuard;
pub use repeat::PadSide;
pub use replace::ReplaceMode;
pub use slice::TrimSides;
pub use splice::StringSplicer;

#[derive(Default)]
pub(super) struct State {
    recursion: recursion::Visited,
    splits: split::SplitCache,
}

impl State {
    pub(super) fn clear_caches(&self) {
        self.splits.clear();
    }
}
