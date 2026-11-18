//! Adds instructions, and keeps track of what they tell and what they change.

use bun_engine_bytecode::{Operand, Register};
use bun_engine_codegen::ir::{
    self, Access, AccessFlags, EffectFlags, Heap, Op, State, Type, Value as Ssa,
};
use bun_engine_vm::{SpeculatedType as T, Value};

use super::Builder;
use crate::graph::{ElementCheck, ExitKind, Gives, Kind, Known, ShapeSet, heaps};

impl Builder<'_> {
    #[inline]
    pub(super) fn ir(&mut self) -> ir::Builder<'_> {
        let mut builder = ir::Builder::at_end(&mut self.graph.function, self.block);
        builder.origin = self.origin;
        builder
    }

    #[inline]
    pub(super) fn insert_node(
        &mut self,
        kind: Kind,
        operands: &[Ssa],
        data: u32,
        state: State,
    ) -> Ssa {
        self.ir().node(
            kind.id(),
            kind.gives().machine_type(),
            operands,
            data,
            state,
        )
    }

    pub(super) fn node(&mut self, kind: Kind, operands: &[Ssa], data: u32) -> Ssa {
        let effects = kind.effects();
        let state = if effects.has(EffectFlags::EXITS) {
            self.state()
        } else {
            State::NONE
        };
        let value = self.insert_node(kind, operands, data, state);
        self.knowledge
            .note_effects(&self.graph, effects, kind.can_collect());
        if self.graph.function.may_alias(effects.writes, heaps::WORLD)
            && effects.writes != heaps::STRING_FIBER
        {
            self.note_effect();
            self.needs_invalidation_point |= self
                .graph
                .function
                .may_alias(effects.writes, heaps::WATCHPOINT_STATES);
        }
        if let Gives::Value(ty) = kind.gives() {
            self.graph.predict(value, ty);
        }
        value
    }

    /// For a node whose result has been profiled.
    pub(super) fn profiled_node(
        &mut self,
        kind: Kind,
        operands: &[Ssa],
        data: u32,
        prediction: T,
    ) -> Ssa {
        let value = self.node(kind, operands, data);
        self.graph.predict(value, prediction);
        value
    }

    // Registers and constants

    pub(super) fn get(&mut self, operand: impl Into<Operand>) -> Ssa {
        let operand = operand.into();
        match operand.as_constant() {
            Some(index) => self.value_or_empty(self.frame().code_block.constant(index)),
            None => self.frame().get(operand.as_register().unwrap()),
        }
    }

    pub(super) fn set(&mut self, register: Register, value: Ssa) {
        self.frame_mut().set(register, value);
    }

    pub(super) fn get_run(&mut self, first: Register, count: u32) -> smallvec::SmallVec<[Ssa; 8]> {
        (0..count)
            .map(|index| self.frame().get(first.nth(index)))
            .collect()
    }

    pub(super) fn undefined(&mut self) -> Ssa {
        self.value(Value::UNDEFINED)
    }

    pub(super) fn empty(&mut self) -> Ssa {
        self.graph.function.const64(0)
    }

    pub(super) fn word(&mut self, word: usize) -> Ssa {
        self.graph.function.const64(word as i64)
    }

    pub(super) fn int32(&mut self, value: i32) -> Ssa {
        self.graph.function.const32(value)
    }

    pub(super) fn prediction_at(&self, metadata: u32) -> T {
        self.frame().code_block.prediction(metadata).get()
    }

    /// Exits if the instruction has never produced a value.
    pub(super) fn prediction_or_exit(&mut self, metadata: u32) -> Option<T> {
        let prediction = self.prediction_at(metadata);
        if prediction.is_empty() && !self.has_exited(ExitKind::NeverExecuted) {
            self.force_exit(ExitKind::NeverExecuted);
            return None;
        }
        Some(if prediction.is_empty() {
            T::HEAP_TOP
        } else {
            prediction
        })
    }

    pub(super) fn predict_entry_values(&mut self, registers: &[Register]) {
        let code_block = self.frame().code_block;
        for &register in registers {
            let value = self.frame().get(register);
            let prediction = match register.offset() - Register::THIS.offset() {
                index @ 0.. => code_block.argument_prediction(index as u32).get(),
                _ if register == Register::CALLEE => {
                    self.knowledge
                        .learn_type(&self.graph, value, T::OBJECT, value);
                    T::FUNCTION
                }
                _ => T::empty(),
            };
            self.graph.predict(value, prediction);
        }
    }

    /// What is expected of a value, as far as can be told before the loops are closed.
    pub(super) fn expected(&self, value: Ssa) -> T {
        let root = self.graph.root(value);
        self.graph.prediction(root) & self.knowledge.type_of(&self.graph, root)
    }

    pub(super) fn is(&self, value: Ssa, ty: T) -> bool {
        self.knowledge.is(&self.graph, value, ty)
    }

    // Representations

    pub(super) fn box_boolean(&mut self, boolean: Ssa) -> Ssa {
        match self.graph.function.int_constant(boolean) {
            Some(constant) => self.value(Value::boolean(constant != 0)),
            None => self.node(Kind::BoxBoolean, &[boolean], 0),
        }
    }

    pub(super) fn box_int32(&mut self, int32: Ssa) -> Ssa {
        match self.graph.function.int_constant(int32) {
            Some(constant) => self.value(Value::int32(constant as i32)),
            None => self.node(Kind::BoxInt32, &[int32], 0),
        }
    }

    fn unboxed_int32(&mut self, value: Ssa) -> Option<Ssa> {
        let constant = self
            .graph
            .constant(value)
            .filter(|&bits| bits >= Value::NUMBER_TAG);
        constant
            .map(|bits| self.int32(bits as i32))
            .or_else(|| self.graph.inside(value, Kind::BoxInt32))
    }

    pub(super) fn as_int32(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        let known = self
            .knowledge
            .get(root)
            .and_then(|known| known.int32.expand());
        known
            .or_else(|| self.unboxed_int32(root))
            .unwrap_or_else(|| {
                let int32 = self.node(Kind::AsInt32, &[root], 0);
                self.knowledge.entry(&self.graph, root).int32 = int32;
                int32
            })
    }

    pub(super) fn truncate(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        let known = self
            .knowledge
            .get(root)
            .and_then(|known| known.int32.expand());
        let unsigned = || self.graph.inside(root, Kind::UInt32ToNumber);
        known
            .or_else(unsigned)
            .or_else(|| self.unboxed_int32(root))
            .unwrap_or_else(|| self.node(Kind::Truncate, &[root], 0))
    }

    pub(super) fn as_double(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        let known = self
            .knowledge
            .get(root)
            .and_then(|known| known.double.expand());
        known
            .or_else(|| self.graph.inside(root, Kind::BoxDouble))
            .unwrap_or_else(|| {
                let double = self.node(Kind::AsDouble, &[root], 0);
                self.knowledge.entry(&self.graph, root).double = double;
                double
            })
    }

    pub(super) fn to_boolean(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        if let Some(truth) = self.truth_of_constant(root) {
            return self.int32(truth.into());
        }
        self.graph
            .inside(root, Kind::BoxBoolean)
            .unwrap_or_else(|| self.node(Kind::ToBoolean, &[root], 0))
    }

    pub(super) fn not(&mut self, boolean: Ssa) -> Ssa {
        self.ir().xor(boolean, 1)
    }

    pub(super) fn is_type(&mut self, value: Ssa, ty: T) -> Ssa {
        let known = self.knowledge.type_of(&self.graph, value);
        if ty.contains(known) || !ty.intersects(known) {
            return self.int32(ty.contains(known).into());
        }
        let (root, data) = (self.graph.root(value), self.graph.intern_type(ty));
        self.node(Kind::IsType, &[root], data)
    }

    // Checks

    /// The value in a form that is tied to the place from where what is known of it holds.
    pub(super) fn alias(&mut self, value: Ssa) -> Ssa {
        let root = self.graph.root(value);
        let Some(&Known { alias, ty, .. }) = self.knowledge.get(root) else {
            return root;
        };
        alias.expand().unwrap_or_else(|| {
            let data = self.graph.intern_type(ty);
            let alias = self.insert_node(Kind::Refine, &[root], data, State::NONE);
            self.knowledge.entry(&self.graph, root).alias = alias;
            alias
        })
    }

    pub(super) fn check_type(&mut self, value: Ssa, ty: T) -> Ssa {
        let alias = self.alias(value);
        if self.is(value, ty) {
            return alias;
        }
        let data = self.graph.intern_type(ty);
        let checked = self.node(Kind::CheckType, &[alias], data);
        self.knowledge.learn_type(&self.graph, value, ty, checked);
        checked
    }

    pub(super) fn check_shapes(&mut self, value: Ssa, shapes: ShapeSet) -> Ssa {
        let known = self.knowledge.shapes_of(&mut self.graph, value);
        if self.graph.shapes.is_subset(known, shapes) {
            return self.alias(value);
        }
        let cell = self.check_type(value, T::CELL);
        let checked = self.node(Kind::CheckShape, &[cell], shapes.0);
        let narrowed = self.graph.shapes.intersection(known, shapes);
        self.knowledge
            .learn_shapes(&self.graph, value, narrowed, checked);
        checked
    }

    pub(super) fn check_elements(&mut self, value: Ssa, check: ElementCheck) -> Ssa {
        if self.knowledge.passes(&mut self.graph, value, check) {
            return self.alias(value);
        }
        let cell = self.check_type(value, T::CELL);
        let checked = self.node(Kind::CheckElements, &[cell], check.to_data());
        self.knowledge
            .learn_elements(&self.graph, value, check, checked);
        checked
    }

    pub(super) fn check_identical(&mut self, value: Ssa, constant: Ssa) {
        let root = self.graph.root(value);
        if root != constant {
            self.node(Kind::CheckIdentical, &[root, constant], 0);
        }
    }

    /// Exits if the integer is not zero.
    pub(super) fn exit_if(&mut self, condition: Ssa, kind: ExitKind) {
        if !self.graph.function.is_int_constant(condition, 0) {
            let exit = self.exit(kind);
            self.ir().check(condition, exit);
        }
    }

    pub(super) fn assume_condition(&mut self, condition: Ssa, holds: bool) -> Option<Known> {
        let (subject, ty) = crate::graph::implied_type(&self.graph, condition, holds)?;
        let known = self.knowledge.entry(&self.graph, subject);
        let before = *known;
        known.ty &= ty;
        known.alias = Ssa::NONE;
        Some(before)
    }

    pub(super) fn undo_assumption(&mut self, before: Option<Known>) {
        if let Some(before) = before {
            *self.knowledge.entry(&self.graph, before.root) = before;
        }
    }

    // Memory. The base has passed the checks that make the access valid.

    pub(super) fn load(&mut self, ty: Type, base: Ssa, offset: i32, heap: Heap) -> Ssa {
        let root = self.graph.root(base);
        self.knowledge.field(root, offset, heap).unwrap_or_else(|| {
            let value = self.ir().load(
                ty,
                base,
                offset,
                Access::new(heap, AccessFlags::ALWAYS_READABLE),
            );
            self.knowledge.learn_field(root, offset, heap, value);
            value
        })
    }

    pub(super) fn load_immutable(&mut self, ty: Type, base: Ssa, offset: usize) -> Ssa {
        let (root, offset) = (self.graph.root(base), offset as i32);
        self.knowledge
            .field(root, offset, Heap::NOTHING)
            .unwrap_or_else(|| {
                let access = Access::new(
                    heaps::OTHER_FIELDS,
                    AccessFlags::ALWAYS_READABLE | AccessFlags::IMMUTABLE,
                );
                let value = self.ir().load(ty, base, offset, access);
                self.knowledge
                    .learn_field(root, offset, Heap::NOTHING, value);
                value
            })
    }

    pub(super) fn load_value(&mut self, base: Ssa, offset: i32, heap: Heap, prediction: T) -> Ssa {
        let value = self.load(Type::I64, base, offset, heap);
        if matches!(self.graph.function.op(value), Op::Load { .. })
            && self.graph.prediction(value).is_empty()
        {
            self.graph.predict(value, prediction);
        }
        value
    }

    /// Into an object that nothing else has seen yet.
    pub(super) fn initialize(&mut self, base: Ssa, offset: i32, heap: Heap, value: Ssa) {
        self.ir().store(value, base, offset, heap);
        self.knowledge
            .learn_field(self.graph.root(base), offset, heap, value);
    }

    pub(super) fn store(&mut self, base: Ssa, offset: i32, heap: Heap, value: Ssa) {
        self.knowledge.note_write(&self.graph, heap);
        self.initialize(base, offset, heap, value);
        self.note_effect();
    }

    /// `base` is `owner` or memory that it owns.
    pub(super) fn store_value(
        &mut self,
        owner: Ssa,
        base: Ssa,
        offset: i32,
        heap: Heap,
        value: Ssa,
    ) {
        let value = self.graph.root(value);
        self.store(base, offset, heap, value);
        self.barrier(owner, value);
    }

    pub(super) fn barrier(&mut self, owner: Ssa, value: Ssa) {
        if self
            .knowledge
            .type_of(&self.graph, value)
            .intersects(T::CELL)
        {
            self.insert_node(Kind::StoreBarrier, &[owner, value], 0, State::NONE);
        }
    }

    pub(super) fn heap_member(&mut self, family: heaps::Family, key: usize) -> Heap {
        let number = self.heap_members.len() as u32;
        *self
            .heap_members
            .entry((family, key))
            .or_insert_with(|| heaps::add_member(&mut self.graph.function, family, number))
    }
}
