//! Caches of property lookups that all code shares. An entry is good for one `Vm::prototype_chain_epoch`, so it need
//! not keep any cell alive.

mod copy_plan;
mod has_own_property;
mod megamorphic;

use core::cell::Cell;
use core::mem::offset_of;
use core::ptr::NonNull;

use bun_engine_text::{Identifier, PropertyName, names};

pub(crate) use copy_plan::Move;
pub use has_own_property::{HasOwnPropertyEntry, has_own_property};
pub use megamorphic::{
    LoadEntry, StoreEntry, get_megamorphic, has_megamorphic, primary_hash, put_megamorphic,
    secondary_hash, store_growing,
};
pub(crate) use megamorphic::{has_enumerable_megamorphic, is_known_to_be_absent};

use crate::heap::{Scope, virtual_memory};
use crate::{CellType, JsObject, Vm};

/// Keeps the name alive, so that no other gets its address.
#[repr(transparent)]
struct CachedName(Cell<Option<Identifier>>);

impl CachedName {
    #[inline(always)]
    fn address(&self) -> usize {
        // SAFETY: a pointer or null.
        unsafe { self.0.as_ptr().cast::<*const u8>().read() }.addr()
    }

    #[inline(always)]
    fn is(&self, name: PropertyName<'_>) -> bool {
        self.address() == name.address()
    }

    fn set(&self, name: PropertyName<'_>) {
        if !self.is(name) {
            self.0.set(Some(name.to_identifier()));
        }
    }

    fn take_from(&self, other: &CachedName) {
        self.0.set(other.0.take());
    }

    fn clear(&self) {
        if self.address() != 0 {
            self.0.set(None);
        }
    }
}

#[repr(transparent)]
struct Table<T, const N: usize>([T; N]);

impl<T, const N: usize> Table<T, N> {
    #[inline(always)]
    fn at(&self, hash: u32) -> &T {
        const { assert!(N.is_power_of_two()) };
        &self.0[hash as usize % N]
    }
}

/// All zeros to begin with.
#[repr(C)]
pub struct LookupTables {
    loads: Table<LoadEntry, { LoadEntry::PRIMARY_COUNT }>,
    secondary_loads: Table<LoadEntry, { LoadEntry::SECONDARY_COUNT }>,
    stores: Table<StoreEntry, { StoreEntry::PRIMARY_COUNT }>,
    secondary_stores: Table<StoreEntry, { StoreEntry::SECONDARY_COUNT }>,
    has_own_property: Table<HasOwnPropertyEntry, { HasOwnPropertyEntry::COUNT }>,
}

impl LookupTables {
    pub const OFFSET_OF_LOADS: usize = offset_of!(Self, loads);
    pub const OFFSET_OF_SECONDARY_LOADS: usize = offset_of!(Self, secondary_loads);
    pub const OFFSET_OF_STORES: usize = offset_of!(Self, stores);
    pub const OFFSET_OF_SECONDARY_STORES: usize = offset_of!(Self, secondary_stores);
    pub const OFFSET_OF_HAS_OWN_PROPERTY: usize = offset_of!(Self, has_own_property);

    /// Nothing matches an entry without a name.
    fn clear(&self) {
        let loads = self.loads.0.iter().chain(&self.secondary_loads.0);
        let stores = self.stores.0.iter().chain(&self.secondary_stores.0);
        (loads.map(LoadEntry::name))
            .chain(stores.map(StoreEntry::name))
            .chain(self.has_own_property.0.iter().map(|it| it.name()))
            .for_each(CachedName::clear);
    }
}

pub(crate) struct State {
    tables: NonNull<LookupTables>,
    has_entries: Cell<bool>,
    pub(crate) copy_plans: copy_plan::CopyPlans,
}

const MAPPED_SIZE: usize = size_of::<LookupTables>().next_multiple_of(1 << 16);

impl Default for State {
    fn default() -> State {
        let address = virtual_memory::reserve(MAPPED_SIZE)
            // SAFETY: what has just been reserved.
            .filter(|&address| unsafe { virtual_memory::commit(address, MAPPED_SIZE) })
            .and_then(|address| NonNull::new(core::ptr::with_exposed_provenance_mut(address)));
        State {
            tables: address.unwrap_or_else(|| bun_alloc::out_of_memory()),
            has_entries: Cell::new(false),
            copy_plans: Default::default(),
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release_names();
        // SAFETY: from `reserve`.
        unsafe { virtual_memory::release(self.tables.addr().get(), MAPPED_SIZE) };
    }
}

impl State {
    pub(crate) const OFFSET_OF_TABLES: usize = offset_of!(State, tables);

    #[inline(always)]
    fn tables(&self) -> &LookupTables {
        // SAFETY: mapped for as long as `self`, and zeros are a value of each field.
        unsafe { self.tables.as_ref() }
    }

    fn tables_to_fill(&self) -> &LookupTables {
        self.has_entries.set(true);
        self.tables()
    }

    fn release_names(&self) {
        if self.has_entries.take() {
            self.tables().clear();
        }
    }

    /// Whether a symbol lives on depends on who has its name.
    pub(crate) fn will_collect(&self, scope: Scope) {
        if scope == Scope::Full {
            self.release_names();
        }
    }

    #[cold]
    pub(crate) fn did_wrap_epoch(&self) {
        self.release_names();
        self.copy_plans.clear();
    }
}

impl crate::services::Service for State {}

impl Vm {
    /// The address does not change.
    pub fn lookup_tables(&self) -> &LookupTables {
        self.services.caches.tables()
    }
}

/// Whether the shape and the static tables say all about an own property of that name, which is no index.
fn is_looked_up_in_shape(object: &JsObject, name: PropertyName<'_>) -> bool {
    if !object.overrides_get_own_property_slot() {
        return true;
    }
    match object.cell_type() {
        CellType::Array
        | CellType::DerivedArray
        | CellType::StringObject
        | CellType::DerivedStringObject => name != names::length,
        CellType::Function => {
            name != names::length && name != names::name && name != names::prototype
        }
        CellType::RegExpObject => name != names::lastIndex,
        other if CellType::TYPED_ARRAYS.contains(other) => {
            name.is_symbol() || bun_engine_number::canonical_numeric_index(name.text()).is_none()
        }
        _ => false,
    }
}
