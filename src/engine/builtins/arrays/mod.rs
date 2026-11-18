//! `Array`, `Iterator` and their kin.

pub(crate) mod array;
mod constructor;
mod copy;
pub(crate) mod iterator;
mod iterator_prototype;
mod join;
pub(crate) mod merge_sort;
mod mutate;
mod search;
pub(crate) mod sort;
mod string_sort;

use bun_engine_vm::{JsArray, RealmSlot, RealmWatchpoint, errors};

use crate::prelude::*;
use crate::watchpoints::Conditions;

/// Of an array-like object.
const MAX_LENGTH: u64 = (1 << 53) - 1;

/// ToObject(this value), for a method of `Array.prototype` whose error has its name.
macro_rules! this_object {
    ($cx:ident, $frame:ident, $method:literal) => {
        $frame
            .this_value()
            .require_object_coercible(
                $cx,
                concat!(
                    "Array.prototype.",
                    $method,
                    " requires that |this| not be null or undefined"
                ),
            )
            .and_then(|this| this.to_object($cx))
    };
}

use this_object;

/// Set(object, "length", length, true)
#[inline]
fn set_length(cx: &mut Cx, object: &JsObject, length: u64) -> JsResult<()> {
    match (object.downcast::<JsArray>(), u32::try_from(length)) {
        (Some(array), Ok(length)) => array.set_length(cx, length, true),
        (Some(_), Err(_)) => Err(cx.throw_range_error(errors::INVALID_ARRAY_LENGTH)),
        (None, _) => object.set(cx, names::length, length.into(), true),
    }
    .map(|_| ())
}

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    constructor::initialize_slot(cx, slot) || iterator::initialize_slot(cx, slot)
}

pub(crate) fn add_conditions(
    cx: &Cx,
    which: RealmWatchpoint,
    conditions: &mut Conditions<'_>,
) -> bool {
    let (array, object) = (cx.array_prototype(), cx.object_prototype());
    match which {
        RealmWatchpoint::ArrayPrototypeChainIsSane => {
            conditions.no_elements(&array, object.into());
            conditions.no_elements(&object, Value::NULL);
        }
        RealmWatchpoint::ArrayIteratorProtocol => {
            conditions.pristine(&array, symbols::iterator);
            conditions.pristine_iterator(&cx.array_iterator_prototype());
        }
        RealmWatchpoint::StringIteratorProtocol => {
            conditions.pristine(&cx.string_prototype(), symbols::iterator);
            conditions.pristine_iterator(&cx.string_iterator_prototype());
        }
        RealmWatchpoint::ArraySpecies => {
            conditions.pristine(&array, names::constructor);
            conditions.pristine(&cx.array_constructor(), symbols::species);
        }
        RealmWatchpoint::ArrayJoin => conditions.pristine(&array, names::join),
        RealmWatchpoint::ArrayToString => conditions.pristine(&array, names::toString),
        RealmWatchpoint::ArrayIsConcatSpreadable => {
            conditions.absent(&array, symbols::isConcatSpreadable, object.into());
            conditions.absent(&object, symbols::isConcatSpreadable, Value::NULL);
        }
        _ => return false,
    }
    true
}
