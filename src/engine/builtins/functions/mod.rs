//! `Function`, errors, the functions of the global object, `ShadowRealm` and `console`.

pub(crate) mod error;
pub(crate) mod function;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    use RealmSlot::*;
    match slot {
        error_shape
        | range_error_shape
        | reference_error_shape
        | syntax_error_shape
        | type_error_shape => error::initialize(cx),
        _ => return false,
    }
    true
}

pub(crate) fn add_conditions(_: &Cx, _: RealmWatchpoint, _: &mut Conditions<'_>) -> bool {
    false
}
