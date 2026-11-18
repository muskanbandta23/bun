use bun_engine_codegen::ir::{State, Value as Ssa};
use bun_engine_vm::{CellType, SpeculatedType as T};

use super::Specializer;
use crate::graph::{ElementCheck, ExitKind, Kind, Known, ShapeSet, TypeCheck};

impl Specializer<'_> {
    pub(super) fn type_of(&self, value: Ssa) -> T {
        self.knowledge.type_of(self.graph, value)
    }

    pub(super) fn is(&self, value: Ssa, ty: T) -> bool {
        self.knowledge.is(self.graph, value, ty)
    }

    pub(super) fn expected(&self, value: Ssa) -> T {
        let (root, known) = (self.graph.root(value), self.type_of(value));
        let expected = self.graph.prediction(root) & (known | T::INT52);
        if expected.is_empty() { known } else { expected }
    }

    pub(super) fn alias(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        let Some(&Known { alias, ty, .. }) = self.knowledge.get(root) else {
            return root;
        };
        alias.expand().unwrap_or_else(|| {
            let data = self.graph.intern_type(ty);
            let alias = self.ir().node(
                Kind::Refine.id(),
                bun_engine_codegen::ir::Type::I64,
                &[root],
                data,
                State::NONE,
            );
            self.knowledge.entry(self.graph, root).alias = alias;
            alias
        })
    }

    pub(super) fn check_type(&mut self, value: Ssa, ty: T, failure: ExitKind) -> Ssa {
        let (alias, known) = (self.alias(value), self.type_of(value));
        if ty.contains(known) {
            return alias;
        }
        let data = self.graph.type_check(TypeCheck {
            wanted: ty,
            known,
            failure,
        });
        let checked = self.node(Kind::CheckType, &[alias], data);
        self.knowledge.learn_type(self.graph, value, ty, checked);
        checked
    }

    fn fold_test(&mut self, always: bool, never: bool) {
        if always || never {
            let constant = self.graph.function.const32(always.into());
            self.graph.function.replace(self.at, constant);
        }
    }

    /// Ties the check to the latest form of its operand.
    fn keep_check(&mut self, value: Ssa) {
        let alias = self.alias(value);
        self.graph.function.operands_mut(self.at)[0] = alias;
    }

    fn drop_check(&mut self, value: Ssa) {
        let alias = self.alias(value);
        self.graph.function.replace(self.at, alias);
    }

    pub(super) fn refine(&mut self, kind: Kind, data: u32, first: Ssa, second: Ssa) {
        match kind {
            Kind::CheckType => {
                let check = self.graph.type_check_at(data);
                let checked = self.check_type(first, check.wanted, check.failure);
                self.graph.function.replace(self.at, checked);
            }
            Kind::CheckShape => {
                let known = self.knowledge.shapes_of(self.graph, first);
                if self.graph.shapes.is_subset(known, ShapeSet(data)) {
                    return self.drop_check(first);
                }
                self.keep_check(first);
                let narrowed = self.graph.shapes.intersection(known, ShapeSet(data));
                self.knowledge
                    .learn_shapes(self.graph, first, narrowed, self.at);
            }
            Kind::CheckElements => {
                if self
                    .knowledge
                    .passes(self.graph, first, ElementCheck::from_data(data))
                {
                    return self.drop_check(first);
                }
                self.keep_check(first);
                self.knowledge.learn_elements(
                    self.graph,
                    first,
                    ElementCheck::from_data(data),
                    self.at,
                );
            }
            Kind::CheckIdentical => {
                if self.graph.root(first) == second {
                    return self.drop_check(first);
                }
                self.knowledge.learn_type(
                    self.graph,
                    first,
                    self.graph.inherent_type(second),
                    self.at,
                );
            }
            Kind::CheckExecutable => {
                self.knowledge
                    .learn_type(self.graph, first, T::FUNCTION, self.at)
            }
            Kind::Refine => self.knowledge.learn_type(
                self.graph,
                first,
                self.graph.interned_type(data),
                self.at,
            ),
            Kind::ResolveRope => {
                let resolved = T::ATOM_STRING | T::RESOLVED_STRING;
                if self.is(first, resolved) {
                    return self.drop_check(first);
                }
                let known = self.knowledge.entry(self.graph, self.graph.root(first));
                (known.ty, known.alias) = (resolved, self.at);
            }
            Kind::IsType => {
                let (known, wanted) = (self.type_of(first), self.graph.type_check_at(data).wanted);
                self.fold_test(wanted.contains(known), !wanted.intersects(known));
                if let Some((_, own)) = self.graph.function.op_mut(self.at).node_mut() {
                    *own = self.graph.types.check(TypeCheck {
                        wanted,
                        known,
                        failure: ExitKind::BadType,
                    });
                }
            }
            Kind::IsCellOfType => {
                let (known, wanted) = (self.type_of(first), T::of_cell_type(CellType(data as u8)));
                let is_exact = !wanted.intersects(T::OTHER_OBJECT | T::OTHER_CELL);
                self.fold_test(
                    is_exact && wanted.contains(known),
                    !wanted.intersects(known),
                );
            }
            Kind::SetShape => self.knowledge.note_transition(
                self.graph,
                first,
                self.graph.transitions[data as usize],
            ),
            Kind::StoreBarrier => {
                let owner = self.graph.root(first);
                if self.knowledge.is_young(owner)
                    || !second.is_none() && !self.type_of(second).intersects(T::CELL)
                {
                    return self.graph.function.remove(self.at);
                }
                // With a marker running, each store needs its own.
            }
            Kind::NewObject | Kind::NewScope | Kind::NewFunction => {
                self.knowledge
                    .note_effects(self.graph, kind.effects(), true);
                self.knowledge.learn_young(self.at);
                let shapes = ShapeSet::single(self.graph.allocations[data as usize].shape);
                self.knowledge
                    .learn_shapes(self.graph, self.at, shapes, self.at);
            }
            _ => self
                .knowledge
                .note_effects(self.graph, kind.effects(), kind.can_collect()),
        }
    }
}
