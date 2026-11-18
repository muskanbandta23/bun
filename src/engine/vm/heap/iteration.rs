//! Going through the cells of the heap.

use super::directory::Directory;
use super::{AllocationKind, CellKind, Gc, Heap, IsoSpace, JsCell, MutatorState, space};
use crate::FastHashSet;

fn for_each_live_cell_of(directory: &Directory, visit: &mut impl FnMut(Gc<JsCell>)) {
    if directory.attributes().kind == AllocationKind::Cell {
        for block in directory.blocks() {
            // SAFETY: live.
            block.for_each_live_cell(|cell| visit(unsafe { Gc::from_address(cell) }));
        }
    }
}

impl Heap {
    /// `visit` does not allocate.
    pub fn for_each_live_cell(&self, mut visit: impl FnMut(Gc<JsCell>)) {
        self.stop_allocating();
        let _state = self.enter_state(MutatorState::Collecting);
        self.for_each_directory(|directory| for_each_live_cell_of(directory, &mut visit));
        for allocation in self.precise.borrow().iter() {
            if allocation.attributes.kind == AllocationKind::Cell && allocation.is_live() {
                // SAFETY: live.
                visit(unsafe { Gc::from_address(allocation.cell()) });
            }
        }
    }

    /// `visit` does not allocate.
    pub fn for_each_live_cell_in(&self, space: &IsoSpace, mut visit: impl FnMut(Gc<JsCell>)) {
        space.allocator.stop_allocating(self);
        let _state = self.enter_state(MutatorState::Collecting);
        if let Some(directory) = space.allocator.directory() {
            for_each_live_cell_of(directory, &mut visit);
        }
    }

    /// Of a type that needs no destruction. `visit` does not allocate.
    pub fn for_each_live<T: CellKind>(&self, mut visit: impl FnMut(Gc<T>)) {
        const { assert!(!core::mem::needs_drop::<T>() && size_of::<T>() <= space::LARGE_CUTOFF) };
        let Some(allocator) = self.cells.allocator_for(size_of::<T>()) else {
            return;
        };
        allocator.stop_allocating(self);
        let _state = self.enter_state(MutatorState::Collecting);
        if let Some(directory) = allocator.directory() {
            for_each_live_cell_of(directory, &mut |cell| {
                cell.downcast().into_iter().for_each(&mut visit)
            });
        }
    }

    /// For what runs when marking has ended.
    pub fn for_each_marked_cell_in(&self, space: &IsoSpace, mut visit: impl FnMut(Gc<JsCell>)) {
        let directory = space.allocator.directory();
        for block in directory.into_iter().flat_map(Directory::blocks_with_marks) {
            // SAFETY: marked.
            block.for_each_marked_cell(|cell| visit(unsafe { Gc::from_address(cell) }));
        }
    }

    /// Those that are protected or have a strong handle.
    pub fn for_each_protected_cell(&self, mut visit: impl FnMut(Gc<JsCell>)) {
        let mut cells: FastHashSet<usize> = self.protected.borrow().keys().copied().collect();
        self.strong_set.for_each_cell(|cell| {
            cells.insert(cell);
        });
        let _defer = self.defer_collection();
        for cell in cells {
            // SAFETY: a root.
            visit(unsafe { Gc::from_address(cell) });
        }
    }
}
