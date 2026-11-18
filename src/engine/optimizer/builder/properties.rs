//! Named properties, by what the caches have seen.

use bun_engine_bytecode::{MetadataKind, Operand, Register, op};
use bun_engine_codegen::ir::{Heap, Successor, Type, Value as Ssa};
use bun_engine_text::PropertyName;
use bun_engine_vm::code::metadata::{GetById, GetLength};
use bun_engine_vm::ic::{AccessCases, AccessKind, LoadCache, PrototypeLookup, StoreCache};
use bun_engine_vm::{
    ArrayModes, Attributes, Gc, IndexingMode, JsObject, RootVec, Shape, ShapeId,
    SpeculatedType as T, Storage, WatchpointState, root_vec,
};
use smallvec::SmallVec;

use super::Builder;
use super::frame::Junction;
use crate::graph::{ElementCheck, ExitKind, Kind, ShapeIndex, ShapeSet, Transition, heaps};

#[derive(Clone, Copy, PartialEq)]
enum Load {
    Own(i32),
    Inherited(Gc<PrototypeLookup>),
}

#[derive(Clone, Copy, PartialEq)]
enum Store {
    Replace(i32),
    Add {
        byte_offset: i32,
        to: ShapeIndex,
        lookup: Gc<PrototypeLookup>,
    },
}

/// Shapes that are accessed in the same way.
struct Case<How> {
    shapes: ShapeSet,
    how: How,
}

const MAX_CASES: usize = 8;

fn is_real(shape: ShapeId) -> bool {
    !shape.is_none()
        && shape != bun_engine_vm::ic::NUMBER_SHAPE
        && shape != bun_engine_vm::ic::BOOLEAN_SHAPE
}

fn gave_up(cases: &AccessCases) -> bool {
    cases
        .last()
        .is_some_and(|last| last.kind() != AccessKind::End || last.did_miss())
}

impl Builder<'_> {
    fn named_heap(&mut self, name: PropertyName<'_>) -> Heap {
        self.heap_member(heaps::Family::Named, name.address())
    }

    pub(super) fn storage(&mut self, object: Ssa) -> Ssa {
        self.load(
            Type::I64,
            object,
            JsObject::OFFSET_OF_STORAGE as i32,
            heaps::STORAGE,
        )
    }

    fn slot_base(&mut self, object: Ssa, byte_offset: i32) -> Ssa {
        if byte_offset > 0 {
            object
        } else {
            self.storage(object)
        }
    }

    fn add_case<How: PartialEq>(
        &mut self,
        cases: &mut RootVec<Case<How>>,
        shape: Gc<Shape>,
        how: How,
        merges: bool,
    ) {
        let shape = ShapeSet::single(self.shape(shape));
        match cases.iter_mut().find(|case| merges && case.how == how) {
            Some(case) => case.shapes = self.graph.shapes.union(case.shapes, shape),
            None => cases.push(Case { shapes: shape, how }),
        }
    }

    /// Leaves out the shapes that the object cannot have. False if none is left.
    fn narrow<How>(&mut self, cases: &mut RootVec<Case<How>>, known: ShapeSet) -> bool {
        for case in cases.iter_mut() {
            case.shapes = self.graph.shapes.intersection(case.shapes, known);
        }
        cases.retain(|case| !self.graph.shapes.members(case.shapes).is_empty());
        !cases.is_empty() && cases.len() <= MAX_CASES
    }

    /// Runs `translate` for each case in a block of its own. It gives the result, if there is one and control goes on.
    fn for_each_case<How: Copy>(
        &mut self,
        object: Ssa,
        cases: &[Case<How>],
        mut translate: impl FnMut(&mut Self, Ssa, ShapeSet, How) -> Option<Ssa>,
    ) -> Option<Ssa> {
        if let [only] = cases {
            let object = self.check_shapes(object, only.shapes);
            return translate(self, object, only.shapes, only.how);
        }
        let state = self.state();
        let cell = self.check_type(object, T::CELL);
        let id = self.node(Kind::LoadShapeId, &[cell], 0);
        let blocks: SmallVec<[_; 9]> = (0..=cases.len())
            .map(|_| self.graph.function.new_block())
            .collect();
        let mut keys: SmallVec<[(i64, Successor<'_>); 8]> = SmallVec::new();
        for (case, &block) in cases.iter().zip(&blocks) {
            let ids = self
                .graph
                .shapes
                .members(case.shapes)
                .iter()
                .map(|&shape| self.graph.shapes[shape].id.bits() as i32);
            keys.extend(ids.map(|id| (id.into(), block.into())));
        }
        self.ir().switch(id, &keys, blocks[cases.len()].rare());
        self.block = blocks[cases.len()];
        self.force_exit(ExitKind::BadShape);

        let mut before = Default::default();
        self.knowledge.save(&mut before, |_| true);
        let mut junction = Junction {
            block: Some(self.graph.function.new_block()),
            ..Default::default()
        };
        let mut any_had_effect = false;
        for (case, &block) in cases.iter().zip(&blocks) {
            (self.block, self.checkpoint, self.has_had_effect) = (block, state, false);
            self.knowledge.restore(&self.graph, &before);
            let data = self
                .graph
                .intern_type(self.graph.shapes.type_of(case.shapes));
            let refined = self.insert_node(
                Kind::Refine,
                &[cell],
                data,
                bun_engine_codegen::ir::State::NONE,
            );
            self.knowledge
                .learn_shapes(&self.graph, cell, case.shapes, refined);
            let result = translate(self, refined, case.shapes, case.how);
            if !self.block.is_none() {
                any_had_effect |= self.has_had_effect;
                self.leave_for(&mut junction, result);
            }
        }
        let result = self.enter(&junction)?;
        (self.checkpoint, self.has_had_effect) = (state, false);
        if any_had_effect {
            self.note_effect();
        }
        result.expand()
    }

    // Loads

    fn load_cases(&mut self, cache: &LoadCache) -> Option<RootVec<Case<Load>>> {
        let mut cases = root_vec();
        if !cache.shape().is_none() {
            if !is_real(cache.shape()) {
                return None;
            }
            let how = match cache.inline_lookup() {
                Some(lookup) => Load::Inherited(lookup),
                None => Load::Own(cache.byte_offset()),
            };
            let shape = self.cached_shape(cache.shape());
            self.add_case(&mut cases, shape, how, true);
        }
        if let Some(list) = cache.cases() {
            if gave_up(list) {
                return None;
            }
            for case in list
                .iter()
                .filter(|case| !case.kind().is_last() && case.is_valid())
            {
                let how = match case.kind() {
                    AccessKind::LoadOwn => Load::Own(case.byte_offset()),
                    AccessKind::LoadConstant | AccessKind::LoadInherited => {
                        Load::Inherited(case.lookup()?)
                    }
                    _ => return None,
                };
                if !is_real(case.shape()) {
                    return None;
                }
                let shape = self.cached_shape(case.shape());
                self.add_case(&mut cases, shape, how, true);
            }
        }
        cases
            .iter()
            .all(|case| !matches!(case.how, Load::Inherited(lookup) if !lookup.is_valid()))
            .then_some(cases)
    }

    /// For shapes that the cache of this instruction may not have seen.
    fn own_load_cases(
        &mut self,
        shapes: ShapeSet,
        name: PropertyName<'_>,
    ) -> Option<RootVec<Case<Load>>> {
        let mut cases = root_vec();
        for position in 0..self.graph.shapes.members(shapes).len() {
            let shape = self.shape_at(self.graph.shapes.members(shapes)[position]);
            let (offset, attributes) = shape
                .get(name)
                .filter(|_| shape.property_accesses_are_cacheable() && !shape.is_dictionary())?;
            let is_plain = !shape
                .type_flags()
                .contains(bun_engine_vm::TypeFlags::OVERRIDES_GET_OWN_PROPERTY_SLOT);
            if attributes.intersects(Attributes::ANY_ACCESSOR) || !is_plain {
                return None;
            }
            self.add_case(
                &mut cases,
                shape,
                Load::Own(offset.byte_offset() as i32),
                true,
            );
        }
        Some(cases)
    }

    fn settled_property(
        &mut self,
        object: Ssa,
        shapes: ShapeSet,
        name: PropertyName<'_>,
    ) -> Option<Ssa> {
        let (constant, shape) = (
            self.constant_cell::<JsObject>(object)?,
            self.shape_at(shapes.only()?),
        );
        let (offset, _) = shape.get(name).filter(|_| constant.shape() == shape)?;
        let replacement = shape
            .ensure_replacement_watchpoints(offset)
            .filter(|set| set.state() != WatchpointState::Invalidated)?;
        // Another shape could have another value in the slot by the time the object is back at this one.
        if !self.watch(shape.transition_watchpoints()) {
            return None;
        }
        self.watch_shared(replacement);
        Some(self.value(constant.get_direct_at(offset)))
    }

    pub(super) fn load_own(
        &mut self,
        object: Ssa,
        shapes: ShapeSet,
        name: PropertyName<'_>,
        byte_offset: i32,
        metadata: u32,
    ) -> Option<Ssa> {
        if let Some(value) = self.settled_property(object, shapes, name) {
            return Some(value);
        }
        let (base, heap) = (self.slot_base(object, byte_offset), self.named_heap(name));
        if let Some(known) = self
            .knowledge
            .field(self.graph.root(base), byte_offset, heap)
        {
            return Some(known);
        }
        let prediction = self.prediction_or_exit(metadata)?;
        let value = self.load_value(base, byte_offset, heap, prediction);
        self.knowledge
            .learn_type(&self.graph, value, T::HEAP_TOP, value);
        Some(value)
    }

    fn load_inherited(
        &mut self,
        lookup: Gc<PrototypeLookup>,
        name: PropertyName<'_>,
        metadata: u32,
    ) -> Option<Ssa> {
        self.dependencies.keep(lookup.cell());
        let is_valid = self.watch(lookup.dependents());
        debug_assert!(is_valid);
        match lookup.constant() {
            Some(constant) => Some(self.value(constant)),
            None => {
                let holder = self.cell(lookup.holder().unwrap().cell());
                self.load_own(holder, ShapeSet::ANY, name, lookup.byte_offset(), metadata)
            }
        }
    }

    pub(super) fn get_by_id(
        &mut self,
        dst: Register,
        base: Operand,
        name: PropertyName<'_>,
        cache: &LoadCache,
        metadata: u32,
    ) {
        let object = self.get(base);
        let known = self.knowledge.shapes_of(&mut self.graph, object);
        let proven = if known == ShapeSet::ANY {
            None
        } else {
            self.own_load_cases(known, name)
        };
        let may_speculate =
            !self.has_exited(ExitKind::BadShape) && !self.has_exited(ExitKind::BadType);
        let cases = proven.or_else(|| {
            if may_speculate {
                self.load_cases(cache)
            } else {
                None
            }
        });
        let Some(cases) =
            cases.and_then(|mut cases| self.narrow(&mut cases, known).then_some(cases))
        else {
            return self.interpret_or_exit(metadata);
        };
        let result = self.for_each_case(object, &cases, |builder, object, shapes, how| match how {
            Load::Own(byte_offset) => builder.load_own(object, shapes, name, byte_offset, metadata),
            Load::Inherited(lookup) => builder.load_inherited(lookup, name, metadata),
        });
        if let Some(result) = result {
            self.set(dst, result);
        }
    }

    pub(super) fn get_cached(
        &mut self,
        dst: Register,
        base: Operand,
        name: PropertyName<'_>,
        kind: MetadataKind,
        metadata: u32,
    ) {
        let code_block = self.frame().code_block;
        self.get_by_id(
            dst,
            base,
            name,
            &code_block.checked_metadata::<GetById>(kind, metadata).cache,
            metadata,
        );
    }

    pub(super) fn get_length(&mut self, op: op::GetLength) {
        let code_block = self.frame().code_block;
        let entry = code_block.checked_metadata::<GetLength>(MetadataKind::GetLength, op.metadata);
        let (base, modes) = (self.get(op.base), entry.array.modes());
        let may_speculate =
            !self.has_exited(ExitKind::BadElements) && !self.has_exited(ExitKind::BadType);
        let length = if self.is(base, T::STRING) || may_speculate && modes == ArrayModes::STRING {
            let string = self.check_type(base, T::STRING);
            self.node(Kind::StringLength, &[string], 0)
        } else if may_speculate && modes.contains(ArrayModes::ARRAY) && DENSE_ARRAYS.contains(modes)
        {
            let check = ElementCheck::Mode {
                mask: IndexingMode::IS_ARRAY,
                expected: IndexingMode::IS_ARRAY,
            };
            let check = super::elements::single_kind(modes)
                .map_or(check, |kind| ElementCheck::dense(kind, Some(true), false));
            let array = self.check_elements(base, check);
            let storage = self.storage(array);
            if super::elements::single_kind(modes).is_none() {
                let has_none = self.ir().is_zero(storage);
                self.exit_if(has_none, ExitKind::BadElements);
                let data = self.graph.intern_type(T::all());
                let storage = self.insert_node(
                    Kind::Refine,
                    &[storage],
                    data,
                    bun_engine_codegen::ir::State::NONE,
                );
                let length = self.public_length(storage);
                let is_large = self.ir().less_than(length, 0);
                self.exit_if(is_large, ExitKind::BadElements);
                length
            } else {
                self.public_length(storage)
            }
        } else {
            return self.get_by_id(
                op.dst,
                op.base,
                bun_engine_text::names::length,
                &entry.cache,
                op.metadata,
            );
        };
        let length = self.box_int32(length);
        self.set(op.dst, length);
    }

    pub(super) fn public_length(&mut self, storage: Ssa) -> Ssa {
        self.load(
            Type::I32,
            storage,
            Storage::OFFSET_OF_PUBLIC_LENGTH as i32,
            heaps::PUBLIC_LENGTH,
        )
    }

    // Stores

    fn store_cases(&mut self, cache: &StoreCache) -> Option<RootVec<Case<Store>>> {
        let mut cases = root_vec();
        let mut add = |builder: &mut Self,
                       shape,
                       new_shape: ShapeId,
                       byte_offset,
                       lookup: Option<Gc<PrototypeLookup>>| {
            let shape = builder.cached_shape(shape);
            if new_shape.is_none() {
                builder.add_case(&mut cases, shape, Store::Replace(byte_offset), true);
            } else {
                let new_shape = builder.cached_shape(new_shape);
                let to = builder.shape(new_shape);
                builder.add_case(
                    &mut cases,
                    shape,
                    Store::Add {
                        byte_offset,
                        to,
                        lookup: lookup.filter(|it| it.is_valid())?,
                    },
                    false,
                );
            }
            Some(())
        };
        if !cache.shape().is_none() {
            add(
                self,
                cache.shape(),
                cache.new_shape(),
                cache.byte_offset(),
                cache.inline_lookup(),
            )?;
        }
        if let Some(list) = cache.cases() {
            if gave_up(list) {
                return None;
            }
            for case in list
                .iter()
                .filter(|case| !case.kind().is_last() && case.is_valid())
            {
                match case.kind() {
                    AccessKind::Replace => {
                        add(self, case.shape(), ShapeId::NONE, case.byte_offset(), None)?
                    }
                    AccessKind::Transition | AccessKind::TransitionAndGrow => {
                        add(
                            self,
                            case.shape(),
                            case.new_shape(),
                            case.byte_offset(),
                            case.lookup(),
                        )?;
                    }
                    _ => return None,
                }
            }
        }
        Some(cases)
    }

    pub(super) fn put_by_id(&mut self, op: op::PutById) {
        let code_block = self.frame().code_block;
        let cache = code_block.checked_metadata::<StoreCache>(MetadataKind::PutById, op.metadata);
        if !cache.has_run() && !self.has_exited(ExitKind::NeverExecuted) {
            return self.force_exit(ExitKind::NeverExecuted);
        }
        let (object, value, name) = (
            self.get(op.base),
            self.get(op.value),
            code_block.identifier(op.name),
        );
        let known = self.knowledge.shapes_of(&mut self.graph, object);
        let may_speculate =
            !self.has_exited(ExitKind::BadShape) && !self.has_exited(ExitKind::BadType);
        let cases = if may_speculate {
            self.store_cases(cache)
        } else {
            None
        };
        let Some(cases) =
            cases.and_then(|mut cases| self.narrow(&mut cases, known).then_some(cases))
        else {
            return self.interpret();
        };
        let heap = self.named_heap(name);
        self.for_each_case(object, &cases, |builder, object, shapes, how| {
            match how {
                Store::Replace(byte_offset) => {
                    let base = builder.slot_base(object, byte_offset);
                    builder.store_value(object, base, byte_offset, heap, value);
                }
                Store::Add {
                    byte_offset,
                    to,
                    lookup,
                } => {
                    builder.dependencies.keep(lookup.cell());
                    builder.watch(lookup.dependents());
                    builder.add_property(
                        object,
                        Transition {
                            from: shapes.only().unwrap(),
                            to,
                        },
                        byte_offset,
                        heap,
                        value,
                    );
                }
            }
            None
        });
    }

    fn add_property(
        &mut self,
        object: Ssa,
        transition: Transition,
        byte_offset: i32,
        heap: Heap,
        value: Ssa,
    ) {
        self.graph.transitions.push(transition);
        let data = self.graph.transitions.len() as u32 - 1;
        let capacity = |shape| self.graph.shapes[shape].out_of_line_capacity;
        let base = if capacity(transition.from) == capacity(transition.to) {
            self.slot_base(object, byte_offset)
        } else {
            let storage = self.node(Kind::GrowSlots, &[object], data);
            self.knowledge.learn_field(
                self.graph.root(object),
                JsObject::OFFSET_OF_STORAGE as i32,
                heaps::STORAGE,
                storage,
            );
            storage
        };
        let value = self.graph.root(value);
        self.store(base, byte_offset, heap, value);
        self.insert_node(
            Kind::SetShape,
            &[object],
            data,
            bun_engine_codegen::ir::State::NONE,
        );
        self.knowledge
            .note_transition(&mut self.graph, object, transition);
        self.insert_node(
            Kind::StoreBarrier,
            &[object],
            0,
            bun_engine_codegen::ir::State::NONE,
        );
    }
}

const DENSE_ARRAYS: ArrayModes = ArrayModes::ARRAY
    .union(ArrayModes::UNDECIDED)
    .union(ArrayModes::INT32)
    .union(ArrayModes::DOUBLE)
    .union(ArrayModes::CONTIGUOUS)
    .union(ArrayModes::COPY_ON_WRITE)
    .union(ArrayModes::SLOW_PUT)
    .union(ArrayModes::NON_ORIGINAL_ARRAY_SHAPE);
