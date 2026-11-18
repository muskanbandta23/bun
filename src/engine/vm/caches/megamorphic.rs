//! For sites that see too many shapes or names to remember them. An entry that is pushed out of its place in the
//! primary table gets a second chance in the secondary one.

use core::cell::Cell;
use core::mem::offset_of;

use bun_engine_text::{PropertyName, names};

use super::{CachedName, is_looked_up_in_shape};
use crate::ic::slot_at;
use crate::object::may_be_special_property;
use crate::{
    Attributes, Cx, Gc, GetterSetter, InternalMethod, JsObject, JsResult, PropertySlot, PutKind,
    PutPropertySlot, Shape, ShapeId, TypeFlags, Value, Vm,
};

#[inline(always)]
pub fn primary_hash(shape: ShapeId, name: PropertyName<'_>) -> u32 {
    (shape.bits() >> 4 ^ shape.bits() >> 15).wrapping_add(name.hash())
}

#[inline(always)]
pub fn secondary_hash(shape: ShapeId, name_address: usize) -> u32 {
    let sum = shape.bits().wrapping_add(name_address as u32);
    (sum ^ sum >> 9) >> 3
}

/// What an entry starts with. The first two can be compared as one word.
#[repr(C)]
struct Key {
    shape: Cell<ShapeId>,
    epoch: Cell<u32>,
    name: CachedName,
}

impl Key {
    #[inline(always)]
    fn matches(&self, vm: &Vm, shape: ShapeId, name: PropertyName<'_>) -> bool {
        self.shape.get() == shape
            && self.epoch.get() == vm.prototype_chain_epoch()
            && self.name.is(name)
    }

    fn set(&self, vm: &Vm, shape: ShapeId, name: PropertyName<'_>) {
        self.shape.set(shape);
        self.epoch.set(vm.prototype_chain_epoch());
        self.name.set(name);
    }

    fn take_from(&self, other: &Key) {
        self.shape.set(other.shape.get());
        self.epoch.set(other.epoch.get());
        self.name.take_from(&other.name);
    }
}

macro_rules! entries {
    ($($entry:ident in $primary:ident, $secondary:ident { $($field:ident)* })*) => {$(
        impl $entry {
            pub const PRIMARY_COUNT: usize = 2048;
            pub const SECONDARY_COUNT: usize = 512;
            pub const OFFSET_OF_SHAPE_AND_EPOCH: usize = offset_of!(Self, key.shape);
            pub const OFFSET_OF_NAME: usize = offset_of!(Self, key.name);

            pub(super) fn name(&self) -> &CachedName {
                &self.key.name
            }

            #[inline(always)]
            fn find<'a>(vm: &'a Vm, shape: ShapeId, name: PropertyName<'_>) -> Option<&'a Self> {
                let tables = vm.lookup_tables();
                let entry = tables.$primary.at(primary_hash(shape, name));
                if entry.key.matches(vm, shape, name) {
                    return Some(entry);
                }
                let entry = tables.$secondary.at(secondary_hash(shape, name.address()));
                entry.key.matches(vm, shape, name).then_some(entry)
            }

            /// The caller sets the rest.
            fn claim<'a>(vm: &'a Vm, shape: ShapeId, name: PropertyName<'_>) -> &'a Self {
                let tables = vm.services.caches.tables_to_fill();
                let entry = tables.$primary.at(primary_hash(shape, name));
                if entry.key.epoch.get() == vm.prototype_chain_epoch() {
                    let hash = secondary_hash(entry.key.shape.get(), entry.key.name.address());
                    let second = tables.$secondary.at(hash);
                    second.key.take_from(&entry.key);
                    $(second.$field.set(entry.$field.get());)*
                }
                entry.key.set(vm, shape, name);
                entry
            }
        }

        const _: () = assert!(size_of::<$entry>() == 32);
    )*};
}

entries! {
    LoadEntry in loads, secondary_loads { holder byte_offset accessor_kind is_present is_enumerable }
    StoreEntry in stores, secondary_stores { new_shape byte_offset needs_more_slots }
}

#[repr(C, align(32))]
pub struct LoadEntry {
    key: Key,
    /// Zero for the object itself. For a property that is absent, something that has undefined at the offset.
    holder: Cell<usize>,
    /// As for `ic::slot_at`. Zero if the property has no slot.
    byte_offset: Cell<i32>,
    /// The bits of `Attributes::ANY_ACCESSOR`. Without any, the slot has the value.
    accessor_kind: Cell<u8>,
    is_present: Cell<bool>,
    is_enumerable: Cell<bool>,
}

static HOLDER_OF_UNDEFINED: [u64; 2] = [0, Value::UNDEFINED.to_bits()];

impl LoadEntry {
    pub const OFFSET_OF_HOLDER: usize = offset_of!(Self, holder);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(Self, byte_offset);
    pub const OFFSET_OF_ACCESSOR_KIND: usize = offset_of!(Self, accessor_kind);
    pub const OFFSET_OF_IS_PRESENT: usize = offset_of!(Self, is_present);

    #[inline(always)]
    fn holder_address(&self, first: &JsObject) -> usize {
        match self.holder.get() {
            0 => first.address(),
            holder => holder,
        }
    }

    /// There is a slot.
    #[inline(always)]
    fn stored_value(&self, first: &JsObject) -> Value {
        let (mut base, byte_offset) = (self.holder_address(first), self.byte_offset.get());
        // SAFETY: an object on the chain of `first` that has the slot, or `HOLDER_OF_UNDEFINED`.
        unsafe {
            if byte_offset < 0 {
                base = Gc::<JsObject>::from_address(base).storage().address();
            }
            let address = base.wrapping_add_signed(byte_offset as isize);
            let word = *core::ptr::with_exposed_provenance::<u64>(address);
            Value::from_bits_or_empty(word).unwrap_or_default()
        }
    }

    fn record(vm: &Vm, first: &JsObject, name: PropertyName<'_>, slot: &PropertySlot) {
        let entry = Self::claim(vm, first.shape_id(), name);
        entry.is_present.set(!slot.is_unset());
        (entry.is_enumerable).set(slot.is_enumerable());
        let Some(holder) = slot.base() else {
            entry
                .holder
                .set(HOLDER_OF_UNDEFINED.as_ptr().expose_provenance());
            entry.byte_offset.set(size_of::<u64>() as i32);
            entry.accessor_kind.set(0);
            return;
        };
        let (is_own, offset) = (holder.is_same(first), slot.cached_offset());
        let kind = slot.attributes() & Attributes::ANY_ACCESSOR;
        entry.holder.set(if is_own { 0 } else { holder.address() });
        entry.accessor_kind.set(kind.bits() as u8);
        entry.byte_offset.set(if offset.is_valid() {
            offset.byte_offset() as i32
        } else {
            0
        });
    }
}

/// `get_non_index_property_slot`, which leaves an entry if it can.
fn look_up_and_record(
    cx: &mut Cx,
    first: Gc<JsObject>,
    name: PropertyName<'_>,
    slot: &mut PropertySlot,
) -> JsResult<bool> {
    let epoch = cx.full_prototype_chain_epoch();
    let (mut object, mut is_cacheable, mut first_shape) = (first, true, ShapeId::NONE);
    let found = loop {
        if !is_looked_up_in_shape(&object, name) || object.overrides_get_prototype() {
            return object.get_non_index_property_slot(cx, name, slot);
        }
        let found = object.get_own_non_index_property_slot(cx, &object.shape(), name, slot)?;
        let shape = object.shape();
        is_cacheable &= shape.property_accesses_are_cacheable();
        if object == first {
            first_shape = shape.id();
        }
        if found {
            break true;
        }
        // What is added to a prototype starts a new epoch.
        is_cacheable &= !(object == first && shape.is_dictionary())
            && !(shape.type_flags())
                .contains(TypeFlags::GET_OWN_PROPERTY_SLOT_IS_IMPURE_FOR_ABSENCE);
        match shape.prototype_object() {
            Some(prototype) => object = prototype,
            None => break false,
        }
    };
    if is_cacheable
        && slot.is_cacheable()
        && (slot.cached_offset().is_valid() || !first.shape().is_dictionary())
        && (epoch, first_shape) == (cx.full_prototype_chain_epoch(), first.shape_id())
    {
        LoadEntry::record(cx, &first, name, slot);
    }
    Ok(found)
}

/// GetV
#[inline]
pub fn get_megamorphic(cx: &mut Cx, base: Value, name: PropertyName<'_>) -> JsResult<Value> {
    if let Some(object) = base.as_object()
        && let Some(entry) = LoadEntry::find(cx, object.shape_id(), name)
        && entry.accessor_kind.get() == 0
    {
        return Ok(entry.stored_value(&object));
    }
    get_slow(cx, base, name)
}

#[inline(never)]
fn get_slow(cx: &mut Cx, base: Value, name: PropertyName<'_>) -> JsResult<Value> {
    if name.as_index().is_some() || (name == names::length && base.is_string()) {
        return base.get(cx, name);
    }
    let first = match base.as_object() {
        Some(object) => object,
        None => base.synthesize_prototype(cx)?,
    };
    let mut slot = PropertySlot::new(base, InternalMethod::Get);
    let Some(entry) = LoadEntry::find(cx, first.shape_id(), name) else {
        look_up_and_record(cx, first, name, &mut slot)?;
        return slot.value(cx, name);
    };
    if entry.byte_offset.get() == 0 {
        return first.get_with_receiver(cx, name, base);
    }
    let kind = Attributes::from_bits_retain(entry.accessor_kind.get().into());
    let stored = entry.stored_value(&first);
    if kind.is_empty() {
        return Ok(stored);
    }
    if let Some(accessor) = stored.downcast::<GetterSetter>() {
        return accessor.call_getter(cx, base);
    }
    // SAFETY: the property is present, so this is an object on the chain of `first`.
    let holder = unsafe { Gc::<JsObject>::from_address(entry.holder_address(&first)) };
    holder.fill_accessor_slot(stored, kind, &mut slot);
    slot.value(cx, name)
}

/// HasProperty
#[inline]
pub fn has_megamorphic(cx: &mut Cx, base: Gc<JsObject>, name: PropertyName<'_>) -> JsResult<bool> {
    match LoadEntry::find(cx, base.shape_id(), name) {
        Some(entry) => Ok(entry.is_present.get()),
        None => has_slow(cx, base, name),
    }
}

#[inline(never)]
fn has_slow(cx: &mut Cx, base: Gc<JsObject>, name: PropertyName<'_>) -> JsResult<bool> {
    if name.as_index().is_some() {
        return base.has_property(cx, name);
    }
    let mut slot = PropertySlot::new(base.into(), InternalMethod::HasProperty);
    look_up_and_record(cx, base, name, &mut slot)
}

/// `JsObject::has_enumerable_property`
#[inline]
pub(crate) fn has_enumerable_megamorphic(
    cx: &mut Cx,
    base: Gc<JsObject>,
    name: PropertyName<'_>,
) -> JsResult<bool> {
    if let Some(entry) = LoadEntry::find(cx, base.shape_id(), name) {
        return Ok(entry.is_enumerable.get());
    }
    if name.as_index().is_some() {
        return base.has_enumerable_property(cx, name);
    }
    let mut slot = PropertySlot::new(base.into(), InternalMethod::GetOwnProperty);
    Ok(look_up_and_record(cx, base, name, &mut slot)? && slot.is_enumerable())
}

/// Neither the object nor its prototypes have the property. False if the cache has nothing to say.
#[inline]
pub(crate) fn is_known_to_be_absent(vm: &Vm, object: &JsObject, name: PropertyName<'_>) -> bool {
    LoadEntry::find(vm, object.shape_id(), name).is_some_and(|entry| !entry.is_present.get())
}

#[repr(C, align(32))]
pub struct StoreEntry {
    key: Key,
    /// The same as in the key for a property that exists.
    new_shape: Cell<ShapeId>,
    /// As for `ic::slot_at`.
    byte_offset: Cell<i32>,
    /// A case for `store_growing`.
    needs_more_slots: Cell<bool>,
}

impl StoreEntry {
    pub const OFFSET_OF_NEW_SHAPE: usize = offset_of!(Self, new_shape);
    pub const OFFSET_OF_BYTE_OFFSET: usize = offset_of!(Self, byte_offset);
    pub const OFFSET_OF_NEEDS_MORE_SLOTS: usize = offset_of!(Self, needs_more_slots);
}

/// # Safety
/// The entry matches the object.
#[inline(never)]
pub unsafe fn store_growing(vm: &Vm, object: &JsObject, entry: &StoreEntry, value: Value) {
    // SAFETY: the new shape adds the slot to that of the object, which gets room for it.
    unsafe {
        let (new, byte_offset) = (entry.new_shape.get().decode(), entry.byte_offset.get());
        let old_capacity = object.shape().out_of_line_capacity();
        let storage = object.storage_with_more_slots(vm, old_capacity, new.out_of_line_capacity());
        object.nuke_shape_and_set_storage(storage, false);
        object.add_cached_property(new.id(), byte_offset, value);
    }
}

/// Set(O, P, V, Throw)
#[inline]
pub fn put_megamorphic(
    cx: &mut Cx,
    base: Gc<JsObject>,
    name: PropertyName<'_>,
    value: Value,
    is_strict: bool,
) -> JsResult<()> {
    let Some(entry) = StoreEntry::find(cx, base.shape_id(), name) else {
        return put_slow(cx, base, name, value, is_strict);
    };
    // SAFETY: it matches.
    unsafe {
        if entry.needs_more_slots.get() {
            store_growing(cx, &base, entry, value);
        } else if entry.new_shape.get() == base.shape_id() {
            slot_at(&base, entry.byte_offset.get()).set(&base, Some(value));
        } else {
            base.add_cached_property(entry.new_shape.get(), entry.byte_offset.get(), value);
        }
    }
    Ok(())
}

fn stores_ordinarily(shape: &Shape, name: PropertyName<'_>) -> bool {
    let flags = shape.type_flags();
    !flags.contains(TypeFlags::OVERRIDES_GET_PROTOTYPE)
        && !(flags.contains(TypeFlags::OVERRIDES_PUT)
            && may_be_special_property(shape.instance_type(), name))
}

#[inline(never)]
fn put_slow(
    cx: &mut Cx,
    base: Gc<JsObject>,
    name: PropertyName<'_>,
    value: Value,
    is_strict: bool,
) -> JsResult<()> {
    let (old, epoch) = (base.shape(), cx.full_prototype_chain_epoch());
    let mut slot = PutPropertySlot::new(base.into(), is_strict);
    base.put(cx, name, value, &mut slot)?;
    let new = base.shape();
    let is_cacheable = match slot.cacheable_kind() {
        PutKind::ExistingProperty => old == new && stores_ordinarily(&old, name),
        PutKind::NewProperty => {
            !old.may_be_prototype()
                && !old.has_non_reified_static_properties()
                && old.instance_header() == new.instance_header()
                && old.existing_add_transition(name, Attributes::NONE) == Some(new)
                && (base.chain_direct()).all(|object| stores_ordinarily(&object.shape(), name))
        }
        _ => false,
    };
    if is_cacheable
        && slot.base() == Some(base)
        && old.property_accesses_are_cacheable()
        && epoch == cx.full_prototype_chain_epoch()
    {
        let offset = slot.cached_offset();
        if old == new {
            old.did_cache_property_replacement(cx, offset);
        }
        let entry = StoreEntry::claim(cx, old.id(), name);
        entry.new_shape.set(new.id());
        entry.byte_offset.set(offset.byte_offset() as i32);
        (entry.needs_more_slots).set(old.out_of_line_capacity() != new.out_of_line_capacity());
    }
    Ok(())
}
