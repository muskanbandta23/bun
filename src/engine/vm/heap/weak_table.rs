//! Tables outside the heap whose entries go when the cells that they refer to die.

use core::cell::{Cell, RefCell};

use super::{Heap, Scope, Survivors};
use crate::FastHashSet;

/// A part of the table.
#[repr(C)]
pub struct WeakTable {
    /// Allocates nothing in the heap, and touches no other table.
    prune: unsafe extern "C" fn(&WeakTable, &Survivors),
    has_new_entries: Cell<bool>,
}

impl WeakTable {
    pub const OFFSET_OF_HAS_NEW_ENTRIES: usize = core::mem::offset_of!(WeakTable, has_new_entries);

    pub const fn new(prune: unsafe extern "C" fn(&WeakTable, &Survivors)) -> WeakTable {
        WeakTable {
            prune,
            has_new_entries: Cell::new(false),
        }
    }
}

#[derive(Default)]
pub(super) struct WeakTables {
    all: RefCell<FastHashSet<*const WeakTable>>,
    /// In no other can a young cell be.
    with_new_entries: RefCell<Vec<*const WeakTable>>,
}

impl WeakTables {
    pub(super) fn prune(&self, survivors: &Survivors) {
        let prune = |&table: &*const WeakTable| {
            // SAFETY: promised to `register_weak_table`.
            unsafe {
                (*table).has_new_entries.set(false);
                ((*table).prune)(&*table, survivors);
            }
        };
        let with_new_entries = self.with_new_entries.take();
        match survivors.scope() {
            Scope::Eden => with_new_entries.iter().for_each(prune),
            Scope::Full => self.all.borrow().iter().for_each(prune),
        }
    }
}

impl Heap {
    /// # Safety
    /// The table stays where it is until it is unregistered, and `prune` can be called with it.
    pub unsafe fn register_weak_table(&self, table: &WeakTable) {
        self.weak_tables.all.borrow_mut().insert(table);
    }

    pub fn unregister_weak_table(&self, table: &WeakTable) {
        self.weak_tables.all.borrow_mut().remove(&&raw const *table);
        if table.has_new_entries.take() {
            (self.weak_tables.with_new_entries.borrow_mut())
                .retain(|&it| !core::ptr::eq(it, table));
        }
    }

    /// # Safety
    /// The table is registered.
    #[inline]
    pub unsafe fn did_add_to_weak_table(&self, table: &WeakTable) {
        if !table.has_new_entries.replace(true) {
            self.weak_tables.with_new_entries.borrow_mut().push(table);
        }
    }
}
