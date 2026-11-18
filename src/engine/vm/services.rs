//! The state that the parts of the run time keep in the `Vm`.

use crate::heap::{Scope, Survivors};
use crate::{Visitor, Vm};

#[derive(Default)]
pub(crate) struct Services {}

impl Services {
    pub(crate) fn initialize(&self, _: &Vm) {}

    pub(crate) fn will_enter(&self, _: &Vm) {}

    pub(crate) fn did_wrap_epoch(&self) {}

    pub(crate) fn visit(&self, _: &mut Visitor) {}

    pub(crate) fn reconcile_weak_references(&self, _: &Vm, _: &Survivors) {}

    pub(crate) fn did_collect(&self, _: &Vm, _: Scope) {}

    pub(crate) fn will_destroy(&self, _: &Vm) {}
}
