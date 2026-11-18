//! What the shell keeps for each thread that runs scripts.

use core::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use bun_engine_vm::{FastHashSet, Strong, Vm};

use crate::command_line::CommandLine;
use crate::prelude::*;

#[derive(Default)]
pub(crate) struct State {
    pub(crate) command_line: OnceCell<CommandLine>,
    pub(crate) has_failed: Cell<bool>,
    pub(crate) async_passes: Cell<u32>,
    pub(crate) expected_async_passes: Cell<u32>,
    /// What the shell adds to global objects, which does not show when their properties are listed.
    pub(crate) hidden_globals: RefCell<FastHashSet<Identifier>>,
    /// By when they are due, and then in the order in which they were set.
    timers: RefCell<BTreeMap<(Instant, u64), Strong<Gc<JsObject>>>>,
    timer_count: Cell<u64>,
}

impl State {
    pub(crate) fn of(vm: &Vm) -> Rc<State> {
        vm.extension()
    }

    pub(crate) fn command_line(&self) -> &CommandLine {
        self.command_line.get().expect("set before a script runs")
    }

    pub(crate) fn add_timer(&self, vm: &Vm, due: Instant, callback: Gc<JsObject>) {
        let number = self.timer_count.replace(self.timer_count.get() + 1);
        let mut timers = self.timers.borrow_mut();
        timers.insert((due, number), Strong::new(vm, callback));
    }

    pub(crate) fn next_timer_due(&self) -> Option<Instant> {
        self.timers.borrow().keys().next().map(|key| key.0)
    }

    pub(crate) fn take_next_timer(&self, vm: &Vm) -> Option<Gc<JsObject>> {
        let (_, callback) = self.timers.borrow_mut().pop_first()?;
        Some(callback.get(vm))
    }
}
