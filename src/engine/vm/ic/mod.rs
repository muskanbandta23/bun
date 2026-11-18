//! Inline caches. They are data: the interpreter reads them, baseline code jumps through them, and the optimizer takes
//! them for what is likely.

mod access_case;
mod call_profile;
mod load;
mod prototype_lookup;
mod query;
mod store;

pub use access_case::{AccessCase, AccessCases, AccessKind, Key};
pub(crate) use access_case::CaseArena;
pub use call_profile::{CallProfile, CallTargetCount};
pub use load::{LoadCache, get_by_key, get_own_by_id};
pub use prototype_lookup::PrototypeLookup;
pub(crate) use prototype_lookup::PrototypeLookups;
pub use query::{delete_by_key, has_by_key, instance_of};
pub use store::{StoreCache, StoreMode, put_by_key};

use crate::{JsObject, ShapeId, Value};

/// What a cache takes for the shape of a value that has none.
pub const NUMBER_SHAPE: ShapeId = ShapeId::from_bits(2);
pub const BOOLEAN_SHAPE: ShapeId = ShapeId::from_bits(4);

#[inline(always)]
fn shape_for_lookup(value: Value) -> ShapeId {
    match value.as_cell() {
        Some(cell) => cell.shape_id(),
        None if value.is_number() => NUMBER_SHAPE,
        None if value.is_boolean() => BOOLEAN_SHAPE,
        None => ShapeId::NONE,
    }
}

fn is_live(shape: ShapeId, survivors: &crate::heap::Survivors) -> bool {
    shape.bits() <= BOOLEAN_SHAPE.bits() || survivors.contains_shape(shape)
}

/// # Safety
/// `byte_offset` is that of a slot which the object has.
#[inline(always)]
pub unsafe fn slot_at(object: &JsObject, byte_offset: i32) -> &crate::Field<Option<Value>> {
    let base = if byte_offset > 0 {
        object.address()
    } else {
        object.storage().address()
    };
    let address = base.wrapping_add_signed(byte_offset as isize);
    // SAFETY: the caller's.
    unsafe { &*core::ptr::with_exposed_provenance(address) }
}
