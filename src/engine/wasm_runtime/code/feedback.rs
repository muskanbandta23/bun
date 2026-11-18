//! What baseline code has counted, for the optimizing compiler.

use core::sync::atomic::Ordering;

use bun_engine_wasm_compiler::optimize::{CallFeedback, Feedback, InliningBudget};
use bun_engine_wasm_format::CodeIndex;

use super::CodeGroup;
use super::tiering::entry_increment;
use crate::module::Module;

pub(super) struct Observed<'a> {
    pub(super) module: &'a Module,
    pub(super) group: &'a CodeGroup,
}

impl Feedback for Observed<'_> {
    fn entry_count(&self, code: CodeIndex) -> u32 {
        self.group.existing_function(code).map_or(0, |function| {
            let state = function.state.lock();
            let counted = state
                .heat
                .counted_at_entry(&self.group.slot(code).countdown);
            counted / entry_increment(self.module, code) as u32
        })
    }

    fn call(&self, code: CodeIndex, site: u32) -> CallFeedback {
        let function = self.group.existing_function(code);
        let profile = function.and_then(|function| function.call_profiles.get(site as usize));
        let Some(profile) = profile else {
            return CallFeedback::default();
        };
        let count = profile.count.load(Ordering::Relaxed);
        let hits = count.saturating_sub(profile.misses.load(Ordering::Relaxed));
        let target = self
            .group
            .code_with_slot(profile.slot.load(Ordering::Relaxed));
        CallFeedback {
            count,
            targets: target.map(|target| (target, hits)).into_iter().collect(),
        }
    }

    fn inlining_budget(&self) -> InliningBudget {
        let budget = &self.group.inlining_budget;
        *budget.get_or_init(|| InliningBudget::new(self.module))
    }
}
