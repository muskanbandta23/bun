//! Strings, regular expressions, dates and JSON.

mod date;
pub mod json;
mod regexp;
mod string;

pub use date::DateInstance;
pub use regexp::{RegExp, RegExpGlobalState};
pub use string::*;

#[derive(Default)]
pub(crate) struct State {
    dates: bun_engine_date::DateCache,
    strings: string::State,
}

impl crate::services::Service for State {
    #[inline]
    fn will_enter(&self, vm: &crate::Vm) {
        vm.follow_time_zone();
    }

    fn reconcile_weak_references(&self, _: &crate::Vm, _: &crate::heap::Survivors) {
        self.strings.clear_caches();
    }
}
