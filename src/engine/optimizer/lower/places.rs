//! What marks a place in the code rather than computing anything: exits, fences, counters and the write barrier.

use bun_engine_asm::{Sign, Size};
use bun_engine_codegen::ir::{
    Access, AccessFlags, Action, EffectFlags, Effects, Heap, Input, Op, PatchpointData, Type,
    Value as Ssa,
};
use bun_engine_vm::heap::Heap as VmHeap;
use bun_engine_vm::{CallFrame, CodeBlock, JsCell, Vm};

use super::Lowering;
use super::runtime_calls::Call;
use super::sites::Site;
use crate::builder::Built;
use crate::graph::{ExitKind, heaps};

/// What the interpreter's prologue adds to the counter of a code block for each entry.
const ENTRY_WEIGHT: i32 = 15;

impl Lowering<'_> {
    /// Bytes that are written over with a jump to the exit once an assumption no longer holds.
    pub(super) fn invalidation_point(&mut self) -> Built {
        let token = self.sites.add(Site::InvalidationPoint, self.origin);
        let effects = Effects::with_flags(EffectFlags::EXITS | EffectFlags::KEPT);
        let mut data = PatchpointData::new(Action::Custom, token, effects);
        data.state = self.state;
        self.ir().patchpoint(&data, &[], &[]);
        self.graph.function.remove(self.at);
        Ok(())
    }

    /// The code is left here whatever happens. The node ends its block, so it becomes the end itself.
    pub(super) fn force_exit(&mut self, data: u32) -> Built {
        let kind = ExitKind::from_u8(data as u8);
        let exit = self.exit(kind);
        let always = self.int32(1);
        self.ir().check(always, exit);
        let block = self.block();
        *self.graph.function.op_mut(self.at) = Op::Unreachable;
        self.graph.function.clear_successors(block);
        Ok(())
    }

    /// Lets the run time stop the thread between instructions.
    pub(super) fn poll_traps(&mut self) -> Built {
        let vm = self.vm();
        let access = Access::new(heaps::SIDE_STATE, AccessFlags::empty());
        let pending = self
            .ir()
            .load(Type::I32, vm, Vm::OFFSET_OF_PENDING_TRAPS as i32, access);
        let any = self.ir().is_not_zero(pending);
        self.branch_out(any, Ssa::NONE, true, |lowering, other| {
            let frame = lowering.frame_pointer();
            let target = crate::runtime::operations::address::handle_traps();
            let threw =
                lowering.call_runtime_in(other, target, &[frame], Type::I32, Call::Operation);
            let exit = lowering.exit(ExitKind::Exception);
            lowering.at_end(other).check(threw, exit);
            Ssa::NONE
        });
        Ok(())
    }

    /// Counts a turn of a loop and leaves for the tier that compiles it better once the count is up.
    pub(super) fn tier_up_in_loop(&mut self) -> Built {
        let next = self.count_execution(1);
        let due = self.ir().greater_or_equal(next, 0);
        let exit = self.exit(ExitKind::TierUp);
        self.ir().check(due, exit);
        self.graph.function.remove(self.at);
        Ok(())
    }

    /// Counts a return and starts the next tier where the count is up. The code goes on either way.
    pub(super) fn tier_up_at_return(&mut self, target: usize) -> Built {
        let next = self.count_execution(ENTRY_WEIGHT);
        let due = self.ir().greater_or_equal(next, 0);
        self.branch_out(due, Ssa::NONE, true, |lowering, other| {
            let frame = lowering.frame_pointer();
            lowering.call_runtime_in(other, target, &[frame], Type::Void, Call::Operation);
            Ssa::NONE
        });
        Ok(())
    }

    /// Adds to the counter of the code block that the frame is running, and gives what it holds afterwards.
    fn count_execution(&mut self, weight: i32) -> Ssa {
        let frame = self.frame_pointer();
        let access = Access::new(heaps::FRAME, AccessFlags::ALWAYS_READABLE);
        let code_block = self.ir().load(
            Type::I64,
            frame,
            CallFrame::OFFSET_OF_CODE_BLOCK as i32,
            access,
        );
        let counter = Access::new(heaps::SIDE_STATE, AccessFlags::empty());
        let offset = CodeBlock::OFFSET_OF_EXECUTION_COUNTER as i32;
        let count = self.ir().load(Type::I32, code_block, offset, counter);
        let next = self.ir().add(count, weight);
        self.ir().store(next, code_block, offset, counter);
        next
    }

    /// Follows a store of a cell into another, so that a marker that has been past the owner comes back.
    pub(super) fn store_barrier(&mut self) -> Built {
        let owner = self.operand(0);
        let state = {
            let access = Access::new(heaps::SIDE_STATE, AccessFlags::ALWAYS_READABLE);
            self.ir().load_narrow(
                Size::S8,
                Sign::Unsigned,
                owner,
                JsCell::OFFSET_OF_STATE as i32,
                access,
            )
        };
        let threshold = self.barrier_threshold();
        let needs_it = self.ir().below_or_equal(state, threshold);
        self.branch_out(needs_it, Ssa::NONE, true, |lowering, other| {
            let target = crate::runtime::operations::address::write_barrier_slow();
            lowering.call_runtime_in(other, target, &[owner], Type::Void, Call::Leaf);
            Ssa::NONE
        });
        Ok(())
    }

    /// The byte that a cell's state has to be above for a store into it to need nothing.
    pub(super) fn barrier_threshold(&mut self) -> Ssa {
        let vm = self.vm();
        let offset = (Vm::OFFSET_OF_HEAP + VmHeap::OFFSET_OF_BARRIER_THRESHOLD) as i32;
        let access = Access::new(heaps::SIDE_STATE, AccessFlags::ALWAYS_READABLE);
        self.ir()
            .load_narrow(Size::S8, Sign::Unsigned, vm, offset, access)
    }

    /// Keeps the stores that fill a new cell ahead of the one that lets the collector reach it.
    pub(super) fn publish_fence(&mut self) -> Built {
        if cfg!(target_arch = "x86_64") {
            self.ir().fence(Heap::EVERYTHING, Heap::NOTHING);
            self.graph.function.remove(self.at);
            return Ok(());
        }
        let threshold = self.barrier_threshold();
        let token = self.sites.add(Site::Publish, self.origin);
        let mut effects = Effects::with_flags(EffectFlags::KEPT);
        effects.reads = Heap::EVERYTHING;
        let data = PatchpointData::new(Action::Custom, token, effects);
        self.ir()
            .patchpoint(&data, &[(threshold, Input::Register)], &[]);
        self.graph.function.remove(self.at);
        Ok(())
    }
}
