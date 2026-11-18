//! Making cells: a bump out of an allocator, with a call where its interval is used up.

use bun_engine_codegen::ir::{Access, AccessFlags, Type, Value as Ssa};
use bun_engine_vm::heap::Allocator;
use bun_engine_vm::{Environment, JsCell, JsFunction, JsObject};

use super::Lowering;
use super::runtime_calls::Call;
use crate::builder::{Built, Refusal};
use crate::graph::{Allocation, Transition, heaps};

impl Lowering<'_> {
    /// An object whose slots the stores that follow fill in.
    pub(super) fn new_object(&mut self, data: u32) -> Built {
        let allocation = self.graph.allocations[data as usize];
        let object = self.allocate(allocation)?;
        let empty = self.word(0);
        self.ir().store(
            empty,
            object,
            JsObject::OFFSET_OF_STORAGE as i32,
            heaps::STORAGE,
        );
        let first = size_of::<JsObject>() as i32;
        self.clear_words(object, first, allocation.size as i32, heaps::NAMED);
        self.graph.function.replace(self.at, object);
        Ok(())
    }

    /// A scope, whose parent and template are its operands and whose slots the stores that follow fill in.
    pub(super) fn new_scope(&mut self, data: u32) -> Built {
        let allocation = self.graph.allocations[data as usize];
        let (parent, template) = (self.operand(0), self.operand(1));
        let scope = self.allocate(allocation)?;
        let fields = heaps::INTERNAL_FIELDS;
        self.ir()
            .store(parent, scope, Environment::OFFSET_OF_PARENT as i32, fields);
        self.ir().store(
            template,
            scope,
            Environment::OFFSET_OF_TEMPLATE as i32,
            fields,
        );
        let first = Environment::OFFSET_OF_SLOTS as i32;
        self.clear_words(scope, first, allocation.size as i32, heaps::SCOPE_SLOTS);
        self.graph.function.replace(self.at, scope);
        Ok(())
    }

    /// A function, whose scope and executable are its operands.
    pub(super) fn new_function(&mut self, data: u32) -> Built {
        let allocation = self.graph.allocations[data as usize];
        let (scope, executable) = (self.operand(0), self.operand(1));
        let function = self.allocate(allocation)?;
        let empty = self.word(0);
        self.ir().store(
            empty,
            function,
            JsObject::OFFSET_OF_STORAGE as i32,
            heaps::STORAGE,
        );
        let fields = heaps::INTERNAL_FIELDS;
        self.ir()
            .store(scope, function, JsFunction::OFFSET_OF_SCOPE as i32, fields);
        let target = heaps::FUNCTION_TARGET;
        self.ir().store(
            executable,
            function,
            JsFunction::OFFSET_OF_EXECUTABLE as i32,
            target,
        );
        self.graph.function.replace(self.at, function);
        Ok(())
    }

    /// The address of a new cell with its header written. The node stays, for the fields to be stored before it.
    fn allocate(&mut self, allocation: Allocation) -> Result<Ssa, Refusal> {
        if allocation.allocator == 0 {
            return Err(Refusal::Unsupported);
        }
        let allocator = self.address(allocation.allocator);
        let own = heaps::SIDE_STATE;
        let readable = Access::new(own, AccessFlags::ALWAYS_READABLE);
        let fixed = Access::new(own, AccessFlags::ALWAYS_READABLE | AccessFlags::IMMUTABLE);
        let start = self.ir().load(
            Type::I64,
            allocator,
            Allocator::OFFSET_OF_START as i32,
            readable,
        );
        let end = self.ir().load(
            Type::I64,
            allocator,
            Allocator::OFFSET_OF_END as i32,
            readable,
        );
        // The interval holds one cell of the allocator's own size, which is at least the one that is wanted.
        let cell_size = self.ir().load(
            Type::I32,
            allocator,
            Allocator::OFFSET_OF_CELL_SIZE as i32,
            fixed,
        );
        let stride = self.ir().zero_extend(cell_size);
        let is_used_up = self.ir().above_or_equal(start, end);
        let vm = self.vm();
        let cell = self.branch_before(
            is_used_up,
            Type::I64,
            true,
            |lowering, block| {
                let target = crate::runtime::operations::address::allocate_slow();
                let args = [vm, allocator];
                lowering.call_runtime_in(block, target, &args, Type::I64, Call::Operation)
            },
            |lowering, block| {
                let next = lowering.at_end(block).add(start, stride);
                lowering.at_end(block).store(
                    next,
                    allocator,
                    Allocator::OFFSET_OF_START as i32,
                    own,
                );
                start
            },
        );
        let header = self.graph.shapes[allocation.shape].header;
        let word = self.word(header);
        // Nothing may collect between the header and the fields, so the stores that follow need no barrier.
        self.ir().store(word, cell, 0, heaps::SHAPE);
        Ok(cell)
    }

    /// Writes zeros from `first` up to `end`, so that nothing of the cell is left for the collector to read.
    fn clear_words(&mut self, cell: Ssa, first: i32, end: i32, heap: bun_engine_codegen::ir::Heap) {
        let zero = self.word(0);
        for offset in (first..end).step_by(size_of::<u64>()) {
            self.ir().store(zero, cell, offset, heap);
        }
    }

    /// Moves a cell to the shape that a transition leads to.
    pub(super) fn set_shape(&mut self, data: u32) -> Built {
        let Transition { to, .. } = self.graph.transitions[data as usize];
        let id = self.graph.shapes[to].id;
        let cell = self.operand(0);
        let shape = self.int32(id.bits() as i32);
        self.ir()
            .store(shape, cell, JsCell::OFFSET_OF_SHAPE_ID as i32, heaps::SHAPE);
        self.graph.function.remove(self.at);
        Ok(())
    }
}
