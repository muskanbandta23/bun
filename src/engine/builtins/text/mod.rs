//! `String`, `RegExp` and `JSON`.

mod json;
pub(crate) mod regexp;
pub(crate) mod string;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    json::initialize_slot(cx, slot) || regexp::initialize_slot(cx, slot)
}

pub(crate) fn add_conditions(
    cx: &Cx,
    which: RealmWatchpoint,
    conditions: &mut Conditions<'_>,
) -> bool {
    regexp::add_conditions(cx, which, conditions)
}
