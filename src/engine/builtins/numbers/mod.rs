//! `Number`, `BigInt`, `Math` and `Date`.

mod date_constructor;
mod date_prototype;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    use RealmSlot::*;
    match slot {
        date_prototype | date_constructor | date_shape => self::date_constructor::initialize(cx),
        _ => return false,
    }
    true
}

pub(crate) fn add_conditions(_: &Cx, _: RealmWatchpoint, _: &mut Conditions<'_>) -> bool {
    false
}
