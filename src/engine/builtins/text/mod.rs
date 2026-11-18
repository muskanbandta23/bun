//! `String`, `RegExp`, `JSON` and the escaping of text.

pub(crate) mod string;
mod uri;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    string::initialize_slot(cx, slot) || uri::initialize_slot(cx, slot)
}

pub(crate) fn add_conditions(
    cx: &Cx,
    which: RealmWatchpoint,
    conditions: &mut Conditions<'_>,
) -> bool {
    string::add_conditions(cx, which, conditions)
}
