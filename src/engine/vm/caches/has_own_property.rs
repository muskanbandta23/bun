use core::cell::Cell;
use core::mem::offset_of;

use bun_engine_text::PropertyName;

use super::{CachedName, primary_hash};
use crate::{CellType, Cx, InternalMethod, JsObject, JsResult, PropertySlot, ShapeId};

/// At the `primary_hash`.
#[repr(C)]
pub struct HasOwnPropertyEntry {
    name: CachedName,
    shape: Cell<ShapeId>,
    /// The epoch shifted left by one, and the answer.
    epoch_and_result: Cell<u32>,
}

impl HasOwnPropertyEntry {
    pub const COUNT: usize = 1024;
    pub const OFFSET_OF_NAME: usize = offset_of!(Self, name);
    pub const OFFSET_OF_SHAPE: usize = offset_of!(Self, shape);
    pub const OFFSET_OF_EPOCH_AND_RESULT: usize = offset_of!(Self, epoch_and_result);

    pub(super) fn name(&self) -> &CachedName {
        &self.name
    }
}

/// HasOwnProperty
#[inline]
pub fn has_own_property(cx: &mut Cx, object: &JsObject, name: PropertyName<'_>) -> JsResult<bool> {
    let shape = object.shape_id();
    let entry = (cx.lookup_tables().has_own_property).at(primary_hash(shape, name));
    let result = entry.epoch_and_result.get() ^ cx.prototype_chain_epoch() << 1;
    if entry.shape.get() == shape && entry.name.is(name) && result <= 1 {
        return Ok(result != 0);
    }
    has_own_property_slow(cx, object, name)
}

#[inline(never)]
fn has_own_property_slow(cx: &mut Cx, object: &JsObject, name: PropertyName<'_>) -> JsResult<bool> {
    let mut slot = PropertySlot::new(object.as_value(), InternalMethod::GetOwnProperty);
    let result = object.get_own_property_slot(cx, name, &mut slot)?;
    let shape = object.shape();
    let follows_from_shape = if name.as_index().is_some() {
        object.cell_type() == CellType::FinalObject && !object.indexing_mode().has_elements()
    } else {
        super::is_looked_up_in_shape(object, name)
    };
    if follows_from_shape && shape.property_accesses_are_cacheable() && shape.absence_is_cacheable()
    {
        let table = &cx.services.caches.tables_to_fill().has_own_property;
        let entry = table.at(primary_hash(shape.id(), name));
        entry.name.set(name);
        entry.shape.set(shape.id());
        (entry.epoch_and_result).set(cx.prototype_chain_epoch() << 1 | u32::from(result));
    }
    Ok(result)
}
