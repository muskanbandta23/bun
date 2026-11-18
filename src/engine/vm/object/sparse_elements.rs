use core::cell::Cell;

use bun_engine_macros::{Trace, cell};

use crate::{Attributes, Gc, JsCell, PropertyDescriptor, RootVec, TracedMap, TypeFlags, Value, Vm};

#[derive(Clone, Copy, Trace)]
pub struct SparseEntry {
    /// A `GetterSetter` for an accessor.
    pub value: Value,
    pub attributes: Attributes,
}

impl SparseEntry {
    pub fn descriptor(self) -> PropertyDescriptor {
        PropertyDescriptor::stored(self.value, self.attributes)
    }
}

/// The elements of an object that has few of them for its length, or some with unusual attributes.
#[cell(cell_type = SparseElements, flags = TypeFlags::SHAPE_IS_IMMORTAL)]
pub struct SparseElements {
    base: JsCell,
    pub(self) map: TracedMap<u32, SparseEntry>,
    entries_with_attributes: Cell<u32>,
}

impl SparseElements {
    pub(super) fn create(vm: &Vm) -> Gc<SparseElements> {
        vm.allocate::<SparseElements>(SparseElementsInit {
            base: vm.shapes.sparse_elements(),
            entries_with_attributes: Cell::new(0),
        })
    }

    pub fn len(&self) -> usize {
        self.map().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(super) fn has_only_plain_entries(&self) -> bool {
        self.entries_with_attributes.get() == 0
    }

    #[inline]
    pub fn get(&self, index: u32) -> Option<SparseEntry> {
        self.map().get(&index)
    }

    pub(super) fn insert(&self, index: u32, value: Value, attributes: Attributes) {
        let old = self.map().insert(index, SparseEntry { value, attributes });
        self.did_drop(old);
        if old.is_none() {
            self.heap().did_allocate(size_of::<(u32, SparseEntry)>());
        }
        if !attributes.is_empty() {
            self.entries_with_attributes.update(|count| count + 1);
        }
    }

    pub(super) fn remove(&self, index: u32) {
        self.did_drop(self.map().remove(&index));
    }

    fn did_drop(&self, entry: Option<SparseEntry>) {
        if entry.is_some_and(|entry| !entry.attributes.is_empty()) {
            self.entries_with_attributes.update(|count| count - 1);
        }
    }

    /// In ascending order.
    pub fn sorted_entries(&self) -> RootVec<(u32, SparseEntry)> {
        let mut entries = self.map().entries();
        entries.sort_unstable_by_key(|entry| entry.0);
        entries
    }

    /// In no order.
    pub(crate) fn indices(&self) -> Vec<u32> {
        self.map().keys()
    }

    pub(super) fn entries(&self) -> RootVec<(u32, SparseEntry)> {
        self.map().entries()
    }
}
