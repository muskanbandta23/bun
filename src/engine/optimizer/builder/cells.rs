//! Cells that the code refers to, and the watchpoints that it rests on.

use std::rc::Rc;

use bun_engine_codegen::ir::Value as Ssa;
use bun_engine_vm::{
    Gc, GlobalObject, InlineWatchpointSet, JsCell, JsFunction, JsString, RealmWatchpoint, RootVec,
    Shape, ShapeId, SpeculatedType as T, TypeFlags, Value, WatchpointSet, WatchpointState,
    root_vec,
};

use super::Builder;
use crate::graph::{CellDetail, CellFacts, ShapeFacts, ShapeIndex, ShapeSet};

/// The part of a plan that stays on the thread of the `Vm`.
pub(crate) struct Dependencies {
    /// Kept alive by the code.
    pub(crate) strong: RootVec<Gc<JsCell>>,
    /// The code dies with any of them.
    pub(crate) weak: RootVec<Gc<JsCell>>,
    /// By `ShapeIndex`. Weak too.
    pub(crate) shapes: RootVec<Gc<Shape>>,
    pub(crate) realms: RootVec<Gc<GlobalObject>>,
    pub(crate) watchpoints: Vec<Rc<WatchpointSet>>,
}

impl Default for Dependencies {
    fn default() -> Self {
        Self {
            strong: root_vec(),
            weak: root_vec(),
            shapes: root_vec(),
            realms: root_vec(),
            watchpoints: Vec::new(),
        }
    }
}

impl Dependencies {
    pub(super) fn keep(&mut self, cell: Gc<JsCell>) {
        self.strong.push(cell);
    }
}

impl Builder<'_> {
    pub(super) fn note_realm(&mut self, global: Gc<GlobalObject>) {
        if !self
            .graph
            .realms
            .iter()
            .any(|&(known, _)| known == global.address())
        {
            let holds = |which: &&RealmWatchpoint| {
                global.watchpoints_unchecked(**which).state() == WatchpointState::Watched
            };
            let holding = RealmWatchpoint::ALL
                .iter()
                .filter(holds)
                .fold(0, |bits, &which| bits | 1 << which as u32);
            self.graph.realms.push((global.address(), holding));
            self.dependencies.realms.push(global);
        }
    }

    pub(super) fn global(&self) -> Gc<GlobalObject> {
        self.frame().code_block.global()
    }

    /// Whether the set is valid, in which case the code is invalidated when it fires.
    pub(super) fn watch(&mut self, set: &InlineWatchpointSet) -> bool {
        let is_valid = set.is_still_valid();
        if is_valid {
            self.watch_shared(set.inflate());
        }
        is_valid
    }

    pub(super) fn watch_shared(&mut self, set: Rc<WatchpointSet>) {
        if !self
            .dependencies
            .watchpoints
            .iter()
            .rev()
            .take(8)
            .any(|known| Rc::ptr_eq(known, &set))
        {
            self.dependencies.watchpoints.push(set);
        }
    }

    pub(super) fn watch_realm(&mut self, which: RealmWatchpoint) -> bool {
        let global = self.global();
        self.watch(global.watchpoints(self.vm, which))
    }

    pub(super) fn shape(&mut self, shape: Gc<Shape>) -> ShapeIndex {
        if let Some(&index) = self.shape_indices.get(&shape.id()) {
            return index;
        }
        let global = shape.global();
        let index = self.graph.shapes.add(ShapeFacts {
            id: shape.id(),
            address: Gc::address(shape),
            cell_type: shape.instance_type(),
            ty: T::of_cell_type(shape.instance_type()),
            indexing_mode: shape.instance_indexing_mode(),
            type_flags: shape.type_flags(),
            header: JsCell::header_for(&shape),
            inline_capacity: shape.inline_capacity() as u32,
            out_of_line_capacity: shape.out_of_line_capacity() as u32,
            prototype: shape.prototype().to_bits(),
            global: global.map_or(0, Gc::address),
            is_original_array: global.is_some_and(|global| global.is_original_array_shape(&shape)),
            is_stable: shape.is_worth_watching(),
        });
        self.shape_indices.insert(shape.id(), index);
        self.dependencies.shapes.push(shape);
        index
    }

    /// From a cache, which is cleared when the shape dies.
    pub(super) fn cached_shape(&mut self, id: ShapeId) -> Gc<Shape> {
        crate::runtime::cells::shape_in_cache(id)
    }

    pub(super) fn shape_at(&self, index: ShapeIndex) -> Gc<Shape> {
        self.dependencies.shapes[index.0 as usize]
    }

    pub(super) fn value(&mut self, value: Value) -> Ssa {
        if let Some(cell) = value.as_cell()
            && !self.graph.cells.contains_key(&value.to_bits())
        {
            let facts = self.facts_about(cell);
            self.graph.cells.insert(value.to_bits(), facts);
            self.dependencies.weak.push(cell);
        }
        self.graph.function.const64(value.to_bits() as i64)
    }

    pub(super) fn value_or_empty(&mut self, value: Option<Value>) -> Ssa {
        match value {
            Some(value) => self.value(value),
            None => self.empty(),
        }
    }

    pub(super) fn cell(&mut self, cell: Gc<JsCell>) -> Ssa {
        self.value(cell.as_value())
    }

    fn facts_about(&mut self, cell: Gc<JsCell>) -> CellFacts {
        let shape = cell.shape();
        let is_stable = cell.is_object() && shape.is_worth_watching();
        let detail = if let Some(string) = cell.downcast::<JsString>() {
            CellDetail::String {
                length: string.len(),
            }
        } else if let Some(function) = cell.downcast::<JsFunction>() {
            CellDetail::Function {
                executable: function.executable().address(),
            }
        } else {
            CellDetail::None
        };
        CellFacts {
            ty: T::of_cell(&cell),
            shapes: if is_stable {
                ShapeSet::single(self.shape(shape))
            } else {
                ShapeSet::ANY
            },
            detail,
        }
    }

    pub(super) fn constant_value(&self, value: Ssa) -> Option<Value> {
        crate::runtime::cells::constant_of_graph(
            &self.graph,
            self.graph.constant(self.graph.root(value))?,
        )
    }

    pub(super) fn constant_cell<C: bun_engine_vm::CellKind>(&self, value: Ssa) -> Option<Gc<C>> {
        self.constant_value(value)?.downcast()
    }

    pub(super) fn truth_of_constant(&self, value: Ssa) -> Option<bool> {
        let constant = self.constant_value(value)?;
        match constant.as_cell() {
            Some(cell) if cell.is_object() => (!cell
                .shape()
                .type_flags()
                .contains(TypeFlags::MASQUERADES_AS_UNDEFINED))
            .then_some(true),
            Some(cell) => cell.downcast::<JsString>().map(|string| !string.is_empty()),
            None => Some(match constant.as_number() {
                Some(number) => number != 0.0 && !number.is_nan(),
                None => constant.is_true(),
            }),
        }
    }
}
