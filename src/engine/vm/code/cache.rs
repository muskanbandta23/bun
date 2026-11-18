//! Code of scripts, modules, indirect `eval` and the `Function` constructor, by the text that it was made from.

use core::time::Duration;
use std::rc::Rc;
use std::time::Instant;

use bun_engine_bytecode::{
    Directives, Eval, FunctionInfo, FunctionRef, Instrumentation, Module, Script, StaticScope,
};
use bun_engine_text::StringRef;
use hashbrown::HashTable;

use crate::SourceProvider;

#[derive(Clone)]
pub(super) enum Unit {
    Script(Rc<Script>),
    Module(Rc<Module>),
    Eval(Rc<Eval>),
    Function(FunctionRef, Rc<Directives>),
}

impl Unit {
    pub(super) fn directives(&self) -> &Directives {
        match self {
            Unit::Script(script) => &script.directives,
            Unit::Module(module) => &module.directives,
            Unit::Eval(eval) => &eval.directives,
            Unit::Function(_, directives) => directives,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Goal {
    Script,
    Module,
    Eval,
    Function { parameters_end: Option<u32> },
}

/// All that the code depends on.
#[derive(Clone, Copy)]
pub(super) struct Key<'a> {
    pub(super) provider: &'a SourceProvider,
    pub(super) goal: Goal,
    pub(super) instrumentation: Instrumentation,
    pub(super) outer: Option<&'a Rc<StaticScope>>,
}

struct Entry {
    source: StringRef,
    hash: u32,
    goal: Goal,
    instrumentation: Instrumentation,
    outer: Option<Rc<StaticScope>>,
    unit: Unit,
    last_use: u64,
}

fn spread(hash: u32) -> u64 {
    u64::from(hash).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

impl Entry {
    fn matches(&self, key: &Key<'_>) -> bool {
        self.hash == key.provider.hash()
            && self.goal == key.goal
            && self.instrumentation == key.instrumentation
            && self.outer.as_ref().map(Rc::as_ptr) == key.outer.map(Rc::as_ptr)
            && self.source == *key.provider.source()
    }
}

pub(super) struct CodeCache {
    entries: HashTable<Entry>,
    max_entries: usize,
    /// In code units of source.
    size: usize,
    uses: u64,
    period_start: Instant,
    uses_at_period_start: u64,
}

impl Default for CodeCache {
    fn default() -> CodeCache {
        CodeCache {
            entries: HashTable::new(),
            max_entries: Self::DEFAULT_MAX_ENTRIES,
            size: 0,
            uses: 0,
            period_start: Instant::now(),
            uses_at_period_start: 0,
        }
    }
}

impl CodeCache {
    const DEFAULT_MAX_ENTRIES: usize = 2000;
    const MAX_SIZE: usize = 16_000_000;
    /// What is not used in the whole of one goes.
    const PERIOD: Duration = Duration::from_secs(10);

    pub(super) fn find(&mut self, key: &Key<'_>) -> Option<Unit> {
        let hash = spread(key.provider.hash());
        let entry = self.entries.find_mut(hash, |entry| entry.matches(key))?;
        self.uses += 1;
        entry.last_use = self.uses;
        Some(entry.unit.clone())
    }

    /// `find` has found nothing.
    pub(super) fn add(&mut self, key: &Key<'_>, unit: Unit) {
        self.prune();
        self.uses += 1;
        self.size += key.provider.source().len();
        let entry = Entry {
            source: key.provider.source().clone(),
            hash: key.provider.hash(),
            goal: key.goal,
            instrumentation: key.instrumentation,
            outer: key.outer.cloned(),
            unit,
            last_use: self.uses,
        };
        (self.entries).insert_unique(spread(entry.hash), entry, |entry| spread(entry.hash));
    }

    pub(super) fn clear(&mut self) {
        self.entries = HashTable::new();
        self.size = 0;
    }

    pub(super) fn max_entries(&self) -> usize {
        self.max_entries
    }

    pub(super) fn set_max_entries(&mut self, count: usize) {
        self.max_entries = count.max(Self::DEFAULT_MAX_ENTRIES);
    }

    pub(super) fn function_tables(&self) -> Vec<Rc<[FunctionInfo]>> {
        let table = |entry: &Entry| match &entry.unit {
            Unit::Script(script) => Rc::clone(&script.code.functions),
            Unit::Module(module) => Rc::clone(&module.code.functions),
            Unit::Eval(eval) => Rc::clone(&eval.code.functions),
            Unit::Function(function, _) => Rc::clone(function.table()),
        };
        self.entries.iter().map(table).collect()
    }

    pub(super) fn prune(&mut self) {
        if self.period_start.elapsed() >= Self::PERIOD {
            self.remove_unused_since(self.uses_at_period_start);
            self.period_start = Instant::now();
            self.uses_at_period_start = self.uses;
        }
        while self.entries.len() >= self.max_entries || self.size > Self::MAX_SIZE {
            let mut uses: Vec<u64> = self.entries.iter().map(|entry| entry.last_use).collect();
            let oldest_eighth = uses.len() / 8;
            self.remove_unused_since(*uses.select_nth_unstable(oldest_eighth).1);
        }
    }

    fn remove_unused_since(&mut self, last_use: u64) {
        self.entries.retain(|entry| {
            if entry.last_use <= last_use {
                self.size -= entry.source.len();
            }
            entry.last_use > last_use
        });
    }
}
