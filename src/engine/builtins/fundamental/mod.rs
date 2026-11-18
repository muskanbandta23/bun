//! `Object`, `Reflect`, `Proxy`, `Symbol` and `Boolean`.

pub(crate) mod boolean;
pub(crate) mod object;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    use RealmSlot::*;
    match slot {
        object_prototype_value_of => object::initialize_value_of(cx),
        boolean_prototype | boolean_constructor | boolean_object_shape => boolean::initialize(cx),
        _ => return false,
    }
    true
}

pub(crate) fn add_conditions(
    cx: &Cx,
    which: RealmWatchpoint,
    conditions: &mut Conditions<'_>,
) -> bool {
    match which {
        RealmWatchpoint::ObjectPrototypeChainIsSane => {
            conditions.no_elements(&cx.object_prototype(), Value::NULL);
        }
        _ => return false,
    }
    true
}
