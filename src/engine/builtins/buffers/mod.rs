//! `ArrayBuffer`, typed arrays, `DataView` and `Atomics`.

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(_: &Cx, _: RealmSlot) -> bool {
    false
}

pub(crate) fn add_conditions(_: &Cx, _: RealmWatchpoint, _: &mut Conditions<'_>) -> bool {
    false
}
