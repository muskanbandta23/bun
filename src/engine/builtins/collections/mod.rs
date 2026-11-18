//! `Map`, `Set`, their weak kin, `WeakRef` and `FinalizationRegistry`.

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(_: &Cx, _: RealmSlot) -> bool {
    false
}

pub(crate) fn add_conditions(_: &Cx, _: RealmWatchpoint, _: &mut Conditions<'_>) -> bool {
    false
}
