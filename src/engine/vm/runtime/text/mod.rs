//! Regular expressions, dates and JSON.

mod date;
pub mod json;
mod regexp;

pub use date::DateInstance;
pub use regexp::{RegExp, RegExpGlobalState};

#[derive(Default)]
pub(crate) struct State {
    dates: bun_engine_date::DateCache,
}

impl crate::services::Service for State {
    #[inline]
    fn will_enter(&self, vm: &crate::Vm) {
        vm.follow_time_zone();
    }
}
