//! What is known of values at a point of a walk over the blocks.

use bun_engine_codegen::ir::{Binary, Effects, Heap, Op, Table, Value as Ssa};
use bun_engine_vm::{CellType, SpeculatedType};

use super::{ElementCheck, Graph, Kind, ShapeSet, Transition, heaps};

#[derive(Clone, Copy)]
pub(crate) struct Known {
    pub(crate) root: Ssa,
    /// Stands for the value from where all of this is known. `NONE` if there is no such thing yet.
    pub(crate) alias: Ssa,
    pub(crate) ty: SpeculatedType,
    pub(crate) shapes: ShapeSet,
    /// The shapes are known from before something that can change a shape, so they hold only if they are stable.
    pub(crate) shapes_are_stale: bool,
    pub(crate) elements: Option<ElementCheck>,
    pub(crate) int32: Ssa,
    pub(crate) double: Ssa,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Field {
    base: Ssa,
    offset: i32,
    heap: Heap,
    value: Ssa,
}

const MAX_SAVED: usize = 48;
const MAX_FIELDS: usize = 24;

#[derive(Default)]
pub(crate) struct Knowledge {
    entries: Vec<Known>,
    /// The position in `entries`, if the entry there has this root.
    positions: Table<Ssa, u32>,
    fields: Vec<Field>,
    /// Objects that need no barrier until something can collect.
    young: Vec<Ssa>,
}

#[derive(Default, Clone)]
pub(crate) struct SavedKnowledge {
    entries: Vec<Known>,
    fields: Vec<Field>,
    young: Vec<Ssa>,
}

impl Knowledge {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.fields.clear();
        self.young.clear();
    }

    #[inline]
    fn position(&self, root: Ssa) -> Option<usize> {
        let position = *self.positions.get(root)? as usize;
        self.entries
            .get(position)
            .is_some_and(|entry| entry.root == root)
            .then_some(position)
    }

    #[inline]
    pub(crate) fn get(&self, root: Ssa) -> Option<&Known> {
        self.position(root).map(|position| &self.entries[position])
    }

    fn push(&mut self, known: Known, value_count: usize) -> &mut Known {
        self.positions.grow_to(value_count, u32::MAX);
        self.positions[known.root] = self.entries.len() as u32;
        self.entries.push_mut(known)
    }

    pub(crate) fn entry(&mut self, graph: &Graph, root: Ssa) -> &mut Known {
        match self.position(root) {
            Some(position) => &mut self.entries[position],
            None => self.push(Known::inherent(graph, root), graph.function.value_count()),
        }
    }

    pub(crate) fn type_of(&self, graph: &Graph, value: Ssa) -> SpeculatedType {
        let root = graph.root(value);
        self.get(root)
            .map_or_else(|| graph.inherent_type(root), |known| known.ty)
    }

    pub(crate) fn is(&self, graph: &Graph, value: Ssa, ty: SpeculatedType) -> bool {
        ty.contains(self.type_of(graph, value))
    }

    /// The code counts on the answer.
    pub(crate) fn shapes_of(&self, graph: &mut Graph, value: Ssa) -> ShapeSet {
        let root = graph.root(value);
        let (shapes, is_stale) = self.get(root).map_or_else(
            || (graph.inherent_shapes(root), true),
            |known| (known.shapes, known.shapes_are_stale),
        );
        if is_stale && shapes != ShapeSet::ANY {
            graph.assume_stable(shapes);
        }
        shapes
    }

    pub(crate) fn passes(&self, graph: &mut Graph, value: Ssa, check: ElementCheck) -> bool {
        if self
            .get(graph.root(value))
            .and_then(|known| known.elements)
            .is_some_and(|known| known.implies(check))
        {
            return true;
        }
        let shapes = self.shapes_of(graph, value);
        let holds = |&shape| {
            check.holds_for(
                graph.shapes[shape].cell_type,
                graph.shapes[shape].indexing_mode,
            )
        };
        shapes != ShapeSet::ANY && graph.shapes.members(shapes).iter().all(holds)
    }

    pub(crate) fn learn_type(&mut self, graph: &Graph, value: Ssa, ty: SpeculatedType, alias: Ssa) {
        let known = self.entry(graph, graph.root(value));
        known.ty &= ty;
        known.alias = alias;
    }

    pub(crate) fn learn_shapes(&mut self, graph: &Graph, value: Ssa, shapes: ShapeSet, alias: Ssa) {
        let known = self.entry(graph, graph.root(value));
        known.ty &= graph.shapes.type_of(shapes);
        (known.shapes, known.shapes_are_stale, known.alias) = (shapes, false, alias);
    }

    pub(crate) fn learn_elements(
        &mut self,
        graph: &Graph,
        value: Ssa,
        check: ElementCheck,
        alias: Ssa,
    ) {
        let known = self.entry(graph, graph.root(value));
        known.ty &= match check {
            ElementCheck::Type(cell_type) => SpeculatedType::of_cell_type(cell_type),
            ElementCheck::Mode { .. } => SpeculatedType::OBJECT,
        };
        (known.elements, known.alias) = (Some(check), alias);
    }

    /// Others that had the old shape may be the same object.
    pub(crate) fn note_transition(
        &mut self,
        graph: &mut Graph,
        object: Ssa,
        transition: Transition,
    ) {
        let (from, to) = (
            ShapeSet::single(transition.from),
            ShapeSet::single(transition.to),
        );
        for known in &mut self.entries {
            if known.shapes != ShapeSet::ANY && graph.shapes.is_subset(from, known.shapes) {
                known.shapes = graph.shapes.union(known.shapes, to);
            }
        }
        let known = self.entry(graph, graph.root(object));
        (known.shapes, known.shapes_are_stale) = (to, false);
    }

    pub(crate) fn forget_shapes(&mut self, graph: &Graph) {
        for known in &mut self.entries {
            known.shapes_are_stale = true;
            if !graph.shapes.is_stable(known.shapes) {
                known.shapes = ShapeSet::ANY;
            }
            if let Some(ElementCheck::Mode { .. }) = known.elements {
                known.elements = None;
            }
        }
    }

    pub(crate) fn note_write(&mut self, graph: &Graph, heap: Heap) {
        if graph.function.may_alias(heap, heaps::SHAPE) {
            self.forget_shapes(graph);
        }
        self.fields
            .retain(|field| !graph.function.may_alias(heap, field.heap));
    }

    pub(crate) fn note_effects(&mut self, graph: &Graph, effects: Effects, can_collect: bool) {
        if effects.writes_memory() {
            self.note_write(graph, effects.writes);
        }
        if can_collect {
            self.young.clear();
        }
    }

    /// At the header of a loop of which nothing is known yet.
    pub(crate) fn enter_loop(&mut self, graph: &Graph, params: &[Ssa]) {
        self.note_effects(graph, Effects::CALL, true);
        for &param in params {
            *self.entry(graph, param) = Known::inherent(graph, param);
        }
    }

    // Memory

    pub(crate) fn field(&self, base: Ssa, offset: i32, heap: Heap) -> Option<Ssa> {
        let found = self
            .fields
            .iter()
            .find(|field| (field.base, field.offset, field.heap) == (base, offset, heap));
        found.map(|field| field.value)
    }

    pub(crate) fn learn_field(&mut self, base: Ssa, offset: i32, heap: Heap, value: Ssa) {
        if self.fields.len() == MAX_FIELDS {
            self.fields.remove(0);
        }
        self.fields.push(Field {
            base,
            offset,
            heap,
            value,
        });
    }

    pub(crate) fn is_young(&self, root: Ssa) -> bool {
        self.young.contains(&root)
    }

    pub(crate) fn learn_young(&mut self, root: Ssa) {
        self.young.push(root);
    }

    // Edges

    /// Keeps what is known of the values for which `matters` holds.
    pub(crate) fn save(&self, into: &mut SavedKnowledge, matters: impl Fn(Ssa) -> bool) {
        into.entries.clear();
        into.entries.extend(
            self.entries
                .iter()
                .rev()
                .filter(|known| matters(known.root))
                .take(MAX_SAVED),
        );
        into.entries.reverse();
        into.fields.clone_from(&self.fields);
        into.young.clone_from(&self.young);
    }

    pub(crate) fn restore(&mut self, graph: &Graph, saved: &SavedKnowledge) {
        self.entries.clear();
        for &known in &saved.entries {
            self.push(known, graph.function.value_count());
        }
        self.fields.clone_from(&saved.fields);
        self.young.clone_from(&saved.young);
    }

    /// Leaves in `saved` what is known both there and here.
    pub(crate) fn merge_into(&self, graph: &mut Graph, saved: &mut SavedKnowledge) {
        saved.entries.retain_mut(|there| {
            let Some(here) = self.get(there.root) else {
                return false;
            };
            there.widen(graph, here);
            true
        });
        saved.fields.retain(|field| self.fields.contains(field));
        saved.young.retain(|root| self.young.contains(root));
    }

    /// What can be said of a parameter for which `arg` is passed.
    pub(crate) fn passed_on(&self, graph: &Graph, arg: Ssa, param: Ssa) -> Known {
        let root = graph.root(arg);
        let known = self
            .get(root)
            .copied()
            .unwrap_or_else(|| Known::inherent(graph, root));
        Known {
            root: param,
            alias: param,
            int32: Ssa::NONE,
            double: Ssa::NONE,
            ..known
        }
    }
}

impl Known {
    pub(crate) fn inherent(graph: &Graph, root: Ssa) -> Self {
        Self {
            root,
            alias: root,
            ty: graph.inherent_type(root),
            shapes: graph.inherent_shapes(root),
            shapes_are_stale: true,
            elements: None,
            int32: Ssa::NONE,
            double: Ssa::NONE,
        }
    }

    pub(crate) fn widen(&mut self, graph: &mut Graph, other: &Known) {
        let common = |a: Ssa, b: Ssa| if a == b { a } else { Ssa::NONE };
        self.ty |= other.ty;
        self.shapes = graph.shapes.union(self.shapes, other.shapes);
        self.shapes_are_stale |= other.shapes_are_stale;
        if self.shapes_are_stale && !graph.shapes.is_stable(self.shapes) {
            self.shapes = ShapeSet::ANY;
        }
        self.elements = self.elements.zip(other.elements).and_then(|(a, b)| {
            if a.implies(b) {
                Some(b)
            } else {
                b.implies(a).then_some(a)
            }
        });
        self.alias = common(self.alias, other.alias);
        self.int32 = common(self.int32, other.int32);
        self.double = common(self.double, other.double);
    }
}

impl SavedKnowledge {
    pub(crate) fn find(&self, root: Ssa) -> Option<&Known> {
        self.entries.iter().find(|known| known.root == root)
    }

    pub(crate) fn find_mut(&mut self, root: Ssa) -> Option<&mut Known> {
        self.entries.iter_mut().find(|known| known.root == root)
    }

    pub(crate) fn add(&mut self, known: Known) {
        self.entries.push(known);
    }
}

/// What it says of the type of a value that `condition` holds, or does not.
pub(crate) fn implied_type(
    graph: &Graph,
    mut condition: Ssa,
    mut holds: bool,
) -> Option<(Ssa, SpeculatedType)> {
    use SpeculatedType as T;
    while let Op::Binary(Binary::Xor, [inner, one]) = *graph.function.op(condition)
        && graph.function.is_int_constant(one, 1)
    {
        (condition, holds) = (graph.function.resolve(inner), !holds);
    }
    let (kind, data) = graph.node(condition)?;
    let ty = match (kind, holds) {
        (Kind::IsType, true) => graph.interned_type(data),
        (Kind::IsType, false) => !graph.interned_type(data),
        (Kind::ToBoolean | Kind::GenericToBoolean, true) | (Kind::EqualNull, false) => !T::NULLISH,
        (Kind::ToBoolean | Kind::GenericToBoolean, false) => !T::SYMBOL,
        (Kind::EqualNull, true) => T::NULLISH | T::OBJECT,
        (Kind::IsCellOfType, true) => T::of_cell_type(CellType(data as u8)),
        _ => return None,
    };
    Some((graph.root(graph.operand(condition, 0)), ty))
}
