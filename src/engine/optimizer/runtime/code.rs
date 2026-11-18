//! Optimized code as the rest of the engine sees it.

use core::cell::Cell;

use bun_engine_bytecode::Register;
use bun_engine_codegen::ir::Origin;
use bun_engine_exec_memory::CodeHandle;
use bun_engine_vm::heap::Survivors;
use bun_engine_vm::{
    CallFrame, CodeBlock, CodeOrigin, CompiledCode, Gc, InlineFrame, JsCell, Value, ValueVec,
    Visitor, Vm, Watchpoint,
};

use super::values;
use crate::graph::{CallKind, FrameIndex, FrameInfo, Position};
use crate::lower::output::{CallSite, Output, Place};

/// Machine code for one code block, with the tables that describe its frames.
pub(crate) struct Compiled {
    pub(crate) output: Output,
    pub(crate) handle: CodeHandle,
    /// Addresses of the cells that the code keeps alive, traced through the code block.
    kept: Vec<usize>,
    /// Addresses of the cells that the code only names. It dies with any of them.
    named: Vec<usize>,
    /// In the sets that the code rests on. They fire while this lives, which is while the code block does.
    watchpoints: Vec<Watchpoint>,
    exits_taken: Cell<u32>,
    is_invalidated: Cell<bool>,
}

impl Compiled {
    pub(crate) fn new(
        output: Output,
        handle: CodeHandle,
        kept: Vec<usize>,
        named: Vec<usize>,
        watchpoints: Vec<Watchpoint>,
    ) -> Compiled {
        Compiled {
            output,
            handle,
            kept,
            named,
            watchpoints,
            exits_taken: Cell::new(0),
            is_invalidated: Cell::new(false),
        }
    }

    pub(crate) fn entry(&self) -> usize {
        self.handle.address(self.output.entry)
    }

    /// How often the code has left for the tier below.
    pub(crate) fn exits_taken(&self) -> &Cell<u32> {
        &self.exits_taken
    }

    pub(crate) fn frame_at(&self, frame: FrameIndex) -> &FrameInfo {
        &self.output.frames[frame.index()]
    }

    /// One of the code blocks of `kept`.
    pub(crate) fn code_block_of(&self, frame: FrameIndex) -> Gc<CodeBlock> {
        // SAFETY: `kept` has it and the code block traces them.
        unsafe { Gc::from_address(self.frame_at(frame).code_block) }
    }

    pub(crate) fn position(&self, origin: Origin) -> Position {
        self.output.positions[origin.0 as usize]
    }

    fn origin(&self, position: Position) -> CodeOrigin {
        CodeOrigin {
            bytecode_offset: position.offset,
            inline_frame: (position.frame != FrameIndex::ROOT).then_some(position.frame.0),
        }
    }

    fn call_site(&self, site: u32) -> &CallSite {
        let sites = &self.output.call_sites;
        let found = sites.binary_search_by_key(&site, |call| call.site);
        &sites[found.expect("the code wrote the site of a call it was at")]
    }

    /// What an inlined call was made with. `None` for a register that the layout does not name.
    ///
    /// # Safety
    /// `frame` runs this code and is at a call site inside the inlined call.
    unsafe fn inlined_value(
        &self,
        frame: &CallFrame,
        index: u32,
        wanted: Register,
    ) -> Option<Value> {
        let site = self.call_site(frame.call_site());
        let values =
            &self.output.call_site_values[site.first_value as usize..][..site.value_count as usize];
        let frames = values::frames(&self.output, site.layout, values);
        let state = frames
            .iter()
            .find(|state| state.frame == FrameIndex(index))?;
        let position = state
            .registers
            .iter()
            .position(|&register| register == wanted)?;
        let value = state.values[position];
        debug_assert!(matches!(value.place, Place::Frame(_) | Place::Constant(_)));
        // SAFETY: the caller's: the frame is at this call site, so the slot holds that value.
        let bits = unsafe { values::read(value.place, frame.address(), None) };
        // SAFETY: a tagged value of a live frame.
        unsafe { Value::from_bits_or_empty(values::tagged(bits, value.format)) }
    }
}

impl CompiledCode for Compiled {
    fn origin_of_call_site(&self, call_site: u32) -> CodeOrigin {
        let site = self.call_site(call_site);
        self.origin(Position {
            frame: site.frame,
            offset: site.bytecode_offset,
        })
    }

    fn inline_frame(&self, _vm: &Vm, index: u32) -> InlineFrame {
        let info = self.frame_at(FrameIndex(index));
        InlineFrame {
            code_block: self.code_block_of(FrameIndex(index)),
            caller: self.origin(self.position(info.caller)),
            is_tail_call: info.kind == CallKind::TailCall,
            specialization: info.kind.specialization(),
        }
    }

    unsafe fn inline_callee(&self, frame: &CallFrame, index: u32) -> Value {
        // SAFETY: the caller's.
        unsafe { self.inlined_value(frame, index, Register::CALLEE) }.unwrap_or_default()
    }

    unsafe fn inline_arguments(&self, frame: &CallFrame, index: u32, out: &mut ValueVec) {
        for offset in 0..self.frame_at(FrameIndex(index)).parameters as i32 {
            let register = Register::from_offset(Register::THIS.offset() + offset);
            // SAFETY: the caller's.
            let value = unsafe { self.inlined_value(frame, index, register) };
            out.push(value.unwrap_or_default());
        }
    }

    fn catch_address(&self, call_site: u32, origin: CodeOrigin, target: u32) -> usize {
        let frame = origin.inline_frame.map_or(FrameIndex::ROOT, FrameIndex);
        let found = self.output.handlers.iter().find(|handler| {
            handler.site == call_site && handler.frame == frame && handler.target == target
        });
        self.handle
            .address(found.expect("the compiler found the same handler").offset)
    }

    fn visit(&self, _owner: &CodeBlock, visitor: &mut Visitor) {
        for &address in &self.kept {
            // SAFETY: alive when the code was made, and traced ever since.
            visitor.append(unsafe { Gc::<JsCell>::from_address(address) });
        }
    }

    fn reconcile_weak_references(&self, owner: &CodeBlock, survivors: &Survivors) {
        let is_dead = |&address: &usize| !survivors.contains_address(address);
        if self.named.iter().any(is_dead) {
            owner.jettison(survivors.vm());
        }
    }

    fn invalidate(&self) {
        if self.is_invalidated.replace(true) {
            return;
        }
        for point in &self.output.invalidation_points {
            let at = self.handle.address(point.offset);
            // SAFETY: the code is kept by the handle, and the bytes were left free for a jump. Only this thread runs
            // it, and it is inside the call that invalidates, not at an invalidation point.
            let _ = unsafe {
                bun_engine_exec_memory::replace_with_jump(at, self.handle.address(point.target))
            };
        }
    }

    fn size(&self) -> usize {
        let output = &self.output;
        self.handle.len()
            + size_of_val(&output.call_sites[..])
            + size_of_val(&output.call_site_values[..])
            + size_of_val(&output.exits[..])
            + size_of_val(&output.exit_values[..])
            + size_of_val(&output.handlers[..])
            + size_of_val(&output.layouts[..])
            + size_of_val(&output.layout_registers[..])
            + size_of_val(&output.positions[..])
            + size_of_val(&self.kept[..])
            + size_of_val(&self.named[..])
    }
}
