//! `Array`, `Iterator` and their kin.

pub(crate) mod array;
pub(crate) mod iterator;

use bun_engine_vm::{RealmSlot, RealmWatchpoint};

use crate::prelude::*;
use crate::watchpoints::Conditions;

pub(crate) fn initialize_slot(_: &Cx, _: RealmSlot) -> bool {
    false
}

pub(crate) fn add_conditions(
    cx: &Cx,
    which: RealmWatchpoint,
    conditions: &mut Conditions<'_>,
) -> bool {
    match which {
        RealmWatchpoint::ArrayPrototypeChainIsSane => {
            conditions.no_elements(&cx.array_prototype(), cx.object_prototype().into());
            conditions.no_elements(&cx.object_prototype(), Value::NULL);
        }
        RealmWatchpoint::ArrayIteratorProtocol => {
            conditions.pristine(&cx.array_prototype(), symbols::iterator);
            conditions.pristine(&cx.array_iterator_prototype(), names::next);
        }
        _ => return false,
    }
    true
}
