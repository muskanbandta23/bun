//! Regular expressions.

mod cache;
mod captures;
mod cell;
mod code;
mod exec;
mod global_search;
mod iterator;
mod machine_code;
mod match_all;
mod match_array;
mod object;
mod replace;
mod replace_function;
mod result_cache;
mod splice;
mod split;
mod statics;

pub use captures::{CaptureBuffer, Captures, Match};
pub use cell::RegExp;
pub use exec::{advance_string_index, regexp_exec};
pub use iterator::{RegExpStringIterator, advance_last_index};
pub use match_array::initialize_match_array_slot;
pub use object::RegExpObject;
pub use replace_function::MatchOnRecord;
pub use statics::RegExpGlobalState;

use crate::heap::{Scope, Survivors};
use crate::services::Service;
use crate::{Visitor, Vm};

#[derive(Default)]
pub(super) struct State {
    matcher: code::Matcher,
    cache: cache::Cache,
    /// Lasts until the next full collection.
    replace_cache: result_cache::ResultCache<64>,
    /// Lasts until the next collection.
    split_cache: result_cache::ResultCache<256>,
}

impl Service for State {
    fn initialize(&self, vm: &Vm) {
        self.matcher.initialize(vm);
    }

    fn visit(&self, visitor: &mut Visitor) {
        self.cache.visit(visitor);
        if visitor.scope() == Scope::Eden {
            self.replace_cache.visit(visitor);
        }
    }

    fn reconcile_weak_references(&self, _: &Vm, survivors: &Survivors) {
        self.cache.prune(survivors);
        self.split_cache.clear();
        if survivors.scope() == Scope::Full {
            self.replace_cache.clear();
        }
        self.matcher.trim();
    }
}

impl Vm {
    #[inline(always)]
    fn regexps(&self) -> &State {
        &self.services.text.regexps
    }

    /// What is used again is compiled again.
    pub fn delete_regexp_code(&self) {
        self.regexps().cache.delete_code(self);
        self.regexps().matcher.trim();
    }

    /// The address of the `RegExp` that is searching, or zero. For a thread that has suspended this one.
    pub fn running_regexp(&self) -> usize {
        self.regexps().matcher.running()
    }
}
