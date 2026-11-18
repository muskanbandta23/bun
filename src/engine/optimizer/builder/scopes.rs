//! Variables that are not in registers.

use bun_engine_bytecode::{MetadataKind, Operand, Register, SlotFlags, op};
use bun_engine_codegen::ir::{Heap, Type, Value as Ssa};
use bun_engine_vm::code::metadata::{GetGlobal, GetImport, GlobalAccess};
use bun_engine_vm::{
    Environment, GlobalBinding, JsFunction, SpeculatedType as T, Value, WatchpointState,
};

use super::Builder;
use crate::graph::{ExitKind, Kind, ShapeSet, heaps};
use crate::runtime::cells::binding_in_cache;

impl Builder<'_> {
    pub(super) fn scope_of(&mut self, callee: Ssa) -> Ssa {
        if let Some(function) = self.constant_cell::<JsFunction>(callee) {
            return self.cell(function.scope());
        }
        if self.graph.kind(self.graph.root(callee)) == Some(Kind::NewFunction) {
            return self.graph.operand(self.graph.root(callee), 0);
        }
        let callee = self.alias(callee);
        self.load_immutable(Type::I64, callee, JsFunction::OFFSET_OF_SCOPE)
    }

    pub(super) fn enter_function(&mut self, op: op::Enter) {
        let frame = self.frame();
        let (mut callee, executable) = (frame.get(Register::CALLEE), frame.code_block.executable());
        if let Some(function) = executable.downcast::<bun_engine_vm::FunctionExecutable>()
            && self.constant_value(callee).is_none()
            && let Some(only) = function.singleton()
            && self.watch(function.singleton_watchpoints())
        {
            callee = self.cell(only.cell());
        }
        let scope = self.scope_of(callee);
        self.graph.predict(scope, T::OTHER_CELL);
        self.set(Register::SCOPE, scope);
        let undefined = self.undefined();
        (1..=op.variables).for_each(|index| self.set(Register::local(index), undefined));
    }

    fn outer_scope(&mut self, scope: Register, depth: u32) -> Ssa {
        let mut scope = self.get(scope);
        for _ in 0..depth {
            scope = match self.constant_cell::<Environment>(scope) {
                Some(environment) => self.cell(environment.parent()),
                None if self.graph.kind(scope) == Some(Kind::NewScope) => {
                    self.graph.operand(scope, 0)
                }
                None => self.load_immutable(Type::I64, scope, Environment::OFFSET_OF_PARENT),
            };
        }
        scope
    }

    fn slot(&mut self, slot: u32) -> (i32, Heap) {
        (
            (Environment::OFFSET_OF_SLOTS + slot as usize * 8) as i32,
            self.heap_member(heaps::Family::ScopeSlot, slot as usize),
        )
    }

    fn settled_variable(&self, scope: Ssa, slot: u32) -> Option<Value> {
        let environment = self.constant_cell::<Environment>(scope)?;
        let flags = environment.template().scope().slot_at(slot)?.flags;
        flags
            .intersects(SlotFlags::IMMUTABLE | SlotFlags::WRITTEN_ONCE)
            .then(|| environment.get(slot))?
            .filter(|value| !value.is_undefined())
    }

    /// Without metadata it is nothing that scripts can name.
    pub(super) fn get_scoped(
        &mut self,
        dst: Register,
        scope: Register,
        depth: u32,
        slot: u32,
        metadata: Option<u32>,
    ) -> Option<Ssa> {
        let scope = self.outer_scope(scope, depth);
        let value = match self.settled_variable(scope, slot) {
            Some(value) => self.value(value),
            None => {
                let prediction = match metadata {
                    Some(metadata) => self.prediction_or_exit(metadata)?,
                    None => T::BYTECODE_TOP,
                };
                let (offset, heap) = self.slot(slot);
                self.load_value(scope, offset, heap, prediction)
            }
        };
        self.set(dst, value);
        Some(value)
    }

    /// Returns false if the slot has been found empty before.
    pub(super) fn get_scoped_checked(
        &mut self,
        dst: Register,
        scope: Register,
        depth: u32,
        slot: u32,
        metadata: u32,
    ) -> bool {
        if self.has_exited(ExitKind::BadType) {
            return false;
        }
        self.state();
        if let Some(value) = self.get_scoped(dst, scope, depth, slot, Some(metadata)) {
            self.check_type(value, T::HEAP_TOP);
        }
        true
    }

    pub(super) fn put_scoped(&mut self, scope: Register, depth: u32, slot: u32, value: Operand) {
        let (scope, value) = (self.outer_scope(scope, depth), self.get(value));
        let (offset, heap) = self.slot(slot);
        self.store_value(scope, scope, offset, heap, value);
    }

    pub(super) fn create_scope(&mut self, op: op::CreateScope) {
        let template = self.frame().code_block.scope_template(self.vm, op.scope);
        if template.singleton_watchpoints().is_still_valid() {
            return self.interpret();
        }
        let size = Environment::allocation_size(template.slot_count());
        let Some(allocator) = self.allocator_for(size) else {
            return self.interpret();
        };
        let shape = self.global().environment_shape();
        let operands = [self.get(op.parent), self.cell(template.cell())];
        let data = self.allocation(shape, size, allocator);
        let scope = self.node(Kind::NewScope, &operands, data);
        for slot in 0..template.slot_count() {
            let value = if slot < template.undefined_slots() {
                self.undefined()
            } else {
                self.empty()
            };
            let (offset, heap) = self.slot(slot);
            self.initialize(scope, offset, heap, value);
        }
        self.node(Kind::PublishFence, &[], 0);
        self.set(op.dst, scope);
    }

    fn binding_place(&mut self, binding: &GlobalBinding) -> (Ssa, Heap) {
        let address = core::ptr::from_ref(binding).addr();
        (
            self.word(address),
            self.heap_member(heaps::Family::GlobalBinding, address),
        )
    }

    pub(super) fn get_global(&mut self, op: op::GetGlobal) {
        let code_block = self.frame().code_block;
        let access = &code_block
            .checked_metadata::<GetGlobal>(MetadataKind::GetGlobal, op.metadata)
            .access;
        if let Some(binding) = binding_in_cache(access) {
            if let Some(value) = binding.get()
                && binding.watchpoints().state() == WatchpointState::Watched
                && self.watch(binding.watchpoints())
            {
                let value = self.value(value);
                return self.set(op.dst, value);
            }
            let Some(prediction) = self.prediction_or_exit(op.metadata) else {
                return;
            };
            let (address, heap) = self.binding_place(binding);
            let value = self.load_value(
                address,
                GlobalBinding::OFFSET_OF_VALUE as i32,
                heap,
                prediction,
            );
            if binding.get().is_some() {
                self.knowledge
                    .learn_type(&self.graph, value, T::HEAP_TOP, value);
            }
            self.check_type(value, T::HEAP_TOP);
            return self.set(op.dst, value);
        }
        if access.pointer.get() == 0 || self.has_exited(ExitKind::BadShape) {
            return self.interpret_or_exit(op.metadata);
        }
        let (global, shape) = (self.global(), self.cached_shape(access.shape.get()));
        let object = self.cell(global.cell());
        let shapes = ShapeSet::single(self.shape(shape));
        let object = self.check_shapes(object, shapes);
        let name = code_block.identifier(op.name);
        if let Some(value) =
            self.load_own(object, shapes, name, access.byte_offset.get(), op.metadata)
        {
            self.set(op.dst, value);
        }
    }

    pub(super) fn put_global(&mut self, value: Operand, metadata: u32) {
        let code_block = self.frame().code_block;
        let access = code_block.checked_metadata::<GlobalAccess>(MetadataKind::PutGlobal, metadata);
        let Some(binding) =
            binding_in_cache(access).filter(|binding| !binding.watchpoints().is_still_valid())
        else {
            return self.interpret();
        };
        if !self.watch_realm(bun_engine_vm::RealmWatchpoint::GlobalVariablesAreWritable) {
            return self.interpret();
        }
        let (global, value) = (self.cell(self.global().cell()), self.get(value));
        let (address, heap) = self.binding_place(binding);
        self.store_value(
            global,
            address,
            GlobalBinding::OFFSET_OF_VALUE as i32,
            heap,
            value,
        );
    }

    pub(super) fn get_import(&mut self, op: op::GetImport) {
        let code_block = self.frame().code_block;
        let slot = code_block
            .checked_metadata::<GetImport>(MetadataKind::GetImport, op.metadata)
            .slot
            .get();
        if slot == 0 || self.has_exited(ExitKind::BadType) {
            return self.interpret_or_exit(op.metadata);
        }
        let Some(prediction) = self.prediction_or_exit(op.metadata) else {
            return;
        };
        let address = self.word(slot);
        let value = self.load_value(address, 0, heaps::SCOPE_SLOTS, prediction);
        self.check_type(value, T::HEAP_TOP);
        self.set(op.dst, value);
    }
}
