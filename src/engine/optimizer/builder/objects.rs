//! Making objects.

use bun_engine_bytecode::{MetadataKind, Register, op};
use bun_engine_codegen::ir::Value as Ssa;
use bun_engine_vm::code::metadata::{NewObject, WeakCell};
use bun_engine_vm::{Gc, JsFinalObject, JsFunction, JsObject, Shape, SpeculatedType as T};

use super::Builder;
use crate::graph::{Allocation, ExitKind, Kind, ShapeSet, heaps};

impl Builder<'_> {
    /// The data of a node that makes a cell.
    pub(super) fn allocation(&mut self, shape: Gc<Shape>, size: usize) -> u32 {
        let allocator = self
            .vm
            .heap
            .cells
            .allocator_for(size)
            .map_or(0, |allocator| core::ptr::from_ref(allocator).addr());
        let shape = self.shape(shape);
        self.graph.allocations.push(Allocation {
            shape,
            size: size as u32,
            allocator,
        });
        self.graph.allocations.len() as u32 - 1
    }

    fn new_final_object(&mut self, shape: Gc<Shape>) -> Ssa {
        let data = self.allocation(
            shape,
            JsFinalObject::allocation_size(shape.inline_capacity()),
        );
        let object = self.node(Kind::NewObject, &[], data);
        let shapes = ShapeSet::single(self.shape(shape));
        self.knowledge
            .learn_shapes(&self.graph, object, shapes, object);
        object
    }

    fn profiled_shape(&mut self, metadata: u32) -> Option<Gc<Shape>> {
        let code_block = self.frame().code_block;
        let entry = code_block.checked_metadata::<NewObject>(MetadataKind::NewObject, metadata);
        (!entry.allocator.get().is_null()).then(|| self.cached_shape(entry.shape.get()))
    }

    pub(super) fn new_object(&mut self, op: op::NewObject) {
        let Some(shape) = self.profiled_shape(op.metadata) else {
            return self.interpret();
        };
        let object = self.new_final_object(shape);
        self.node(Kind::PublishFence, &[], 0);
        self.set(op.dst, object);
    }

    pub(super) fn new_object_literal(&mut self, op: op::NewObjectLiteral) {
        let Some(shape) = self.profiled_shape(op.metadata) else {
            return self.interpret();
        };
        let code_block = self.frame().code_block;
        let object = self.new_final_object(shape);
        for (index, name) in code_block.code().name_lists[op.names.index()]
            .iter()
            .enumerate()
        {
            let heap = self.heap_member(heaps::Family::Named, name.name().address());
            let value = self
                .graph
                .root(self.frame().get(op.first.nth(index as u32)));
            self.initialize(
                object,
                (size_of::<JsObject>() + index * 8) as i32,
                heap,
                value,
            );
        }
        self.node(Kind::PublishFence, &[], 0);
        self.set(op.dst, object);
    }

    pub(super) fn create_this(&mut self, op: op::CreateThis) {
        let new_target = self.get(op.new_target);
        let code_block = self.frame().code_block;
        let seen = code_block
            .checked_metadata::<WeakCell>(MetadataKind::CachedCallee, op.metadata)
            .get();
        let seen = seen
            .and_then(Gc::downcast)
            .filter(|_| !self.has_exited(ExitKind::BadIdentity));
        let function = self.constant_cell::<JsFunction>(new_target).or(seen);
        let Some((function, rare_data)) =
            function.and_then(|function| Some((function, function.rare_data()?)))
        else {
            return self.interpret();
        };
        let Some(shape) = rare_data
            .object_shape()
            .filter(|_| self.watch(rare_data.allocation_watchpoints()))
        else {
            return self.interpret();
        };
        let expected = self.cell(function.cell());
        self.check_identical(new_target, expected);
        let object = self.new_final_object(shape);
        self.node(Kind::PublishFence, &[], 0);
        self.set(op.dst, object);
    }

    pub(super) fn new_function(&mut self, op: op::NewFunction) {
        let executable = self.frame().code_block.function(self.vm, op.function);
        if executable.singleton_watchpoints().is_still_valid() {
            return self.interpret();
        }
        let shape = JsFunction::shape_for(self.vm, &self.global(), &executable);
        let operands = [self.get(op.scope), self.cell(executable.cell())];
        let data = self.allocation(shape, size_of::<JsFunction>());
        let function = self.node(Kind::NewFunction, &operands, data);
        self.node(Kind::PublishFence, &[], 0);
        self.set(op.dst, function);
    }

    pub(super) fn to_this(&mut self, this: Register) {
        let value = self.get(this);
        if let Some(constant) = self
            .constant_value(value)
            .filter(|constant| constant.is_undefined_or_null())
        {
            debug_assert!(!constant.is_cell());
            let global_this = self.cell(self.global().global_this().cell());
            return self.set(this, global_this);
        }
        let ordinary = T::OBJECT - T::OTHER_OBJECT;
        let expected = self.expected(value);
        if self.has_exited(ExitKind::BadType) {
            self.interpret();
        } else if ordinary.contains(expected) {
            self.check_type(value, ordinary);
        } else if T::NULLISH.contains(expected) {
            self.check_type(value, T::NULLISH);
            let global_this = self.cell(self.global().global_this().cell());
            self.set(this, global_this);
        } else {
            self.interpret();
        }
    }
}
