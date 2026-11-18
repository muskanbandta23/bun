//! Strings, regular expressions, dates and JSON.

mod date;
pub mod json;
mod regexp;
mod string;

pub use date::DateInstance;
pub use regexp::*;
pub use string::*;

use crate::heap::Survivors;
use crate::{Visitor, Vm};

#[derive(Default)]
pub(crate) struct State {
    dates: bun_engine_date::DateCache,
    regexps: regexp::State,
    strings: string::State,
}

impl crate::services::Service for State {
    fn initialize(&self, vm: &Vm) {
        self.regexps.initialize(vm);
    }

    #[inline]
    fn will_enter(&self, vm: &Vm) {
        vm.follow_time_zone();
    }

    fn visit(&self, visitor: &mut Visitor) {
        self.regexps.visit(visitor);
    }

    fn reconcile_weak_references(&self, vm: &Vm, survivors: &Survivors) {
        self.regexps.reconcile_weak_references(vm, survivors);
        self.strings.clear_caches();
    }
}
