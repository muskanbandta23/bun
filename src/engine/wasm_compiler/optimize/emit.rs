//! The machine code that is not up to the back end.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Code, Scratch};
use bun_engine_codegen::ir::{Origin, Token};
use bun_engine_codegen::{CallSite as CallEnd, Client, Compiled, ExitSite, Frame, PatchpointSite};
use bun_engine_wasm_format::{CodeIndex, TypeIndex};

use super::map::Map;
use super::translator::pins_memory_base;
use crate::abi::{
    Callable, FunctionSlot, INSTANCE, InstanceLayout, MEMORY_BASE, Operation, SLOT, Signature,
    Trap, UNCHECKED_FRAME_SIZE, UNCHECKED_STACK_EXTENSION, callee_word, frame,
};
use crate::output::{CallSite, CompiledFunction, DirectCall, Environment, InlineFrame};
use crate::tail_call;

#[derive(Clone, Copy)]
pub(super) enum Callee {
    Code(CodeIndex),
    /// At this distance from the last operand.
    Callable {
        offset: i32,
    },
}

/// What a patchpoint does.
#[derive(Clone, Copy)]
pub(super) enum Site {
    Nothing,
    Trap(Trap),
    Truncate {
        from: FloatWidth,
        sign: Sign,
        to: Width,
    },
    /// The cell, and the threshold.
    WriteBarrier,
    /// The threshold.
    Publish,
    Call {
        callee: Callee,
        may_leave_instance: bool,
    },
    TailCall(Callee, TypeIndex),
}

/// An origin is zero for the function itself, and otherwise one more than the position of the inline frame.
#[derive(Default)]
pub(super) struct Sites {
    sites: Vec<Site>,
    inline_frames: Vec<InlineFrame>,
    makes_calls: bool,
}

impl Sites {
    pub(super) const NOTHING: Token = Token(0);
    pub(super) const WRITE_BARRIER: Token = Token(1);
    pub(super) const PUBLISH: Token = Token(2);

    pub(super) fn clear(&mut self) {
        self.sites.clear();
        (self.sites).extend([Site::Nothing, Site::WriteBarrier, Site::Publish]);
        self.inline_frames.clear();
        self.makes_calls = false;
    }

    pub(super) fn add(&mut self, site: Site) -> Token {
        self.makes_calls |= matches!(site, Site::Call { .. });
        self.sites.push(site);
        Token(self.sites.len() as u32 - 1)
    }

    pub(super) fn add_inline_frame(&mut self, frame: InlineFrame) -> u32 {
        self.inline_frames.push(frame);
        self.inline_frames.len() as u32 - 1
    }

    pub(super) fn has_inline_frames(&self) -> bool {
        !self.inline_frames.is_empty()
    }

    pub(super) fn inline_frame(origin: Origin) -> u32 {
        origin.0.wrapping_sub(1)
    }

    #[inline]
    pub(super) fn exit_token(trap: Trap, origin: Origin) -> Token {
        Token(origin.0 * Trap::COUNT as u32 + trap as u32)
    }
}

struct Barrier {
    start: Label,
    end: Label,
    cell: Gpr,
    live_registers: RegisterSet,
    live_vectors: RegisterSet,
}

struct Return {
    address: Label,
    handler: Option<Label>,
    origin: Origin,
}

#[derive(Default)]
pub(super) struct Tables {
    /// By the token of the exit.
    traps: Vec<Option<Label>>,
    thunks: Vec<(CodeIndex, Label)>,
    thunk_of: Map<Label>,
    barriers: Vec<Barrier>,
    returns: Vec<Return>,
    direct_calls: Vec<(Label, CodeIndex)>,
}

pub(super) struct Emitter<'a> {
    env: &'a Environment<'a>,
    code: CodeIndex,
    takes_over_frame: bool,
    sites: &'a Sites,
    tables: &'a mut Tables,
    entry: Label,
}

/// For code before and after which no register but those of arguments and results has a value.
const USUAL_SCRATCH: Scratch = Scratch {
    #[cfg(target_arch = "x86_64")]
    gprs: [Some(abi::SCRATCH_GPR), None],
    #[cfg(target_arch = "aarch64")]
    gprs: [Some(abi::SCRATCH_GPRS[0]), Some(abi::SCRATCH_GPRS[1])],
    fprs: [Some(abi::SCRATCH_FPRS[0]), None],
};

fn in_frame(offset: i32) -> Address {
    Address::new(abi::FRAME_POINTER, offset)
}

impl<'a> Emitter<'a> {
    pub(super) fn new(
        env: &'a Environment<'a>,
        code: CodeIndex,
        takes_over_frame: bool,
        sites: &'a Sites,
        tables: &'a mut Tables,
        masm: &mut MacroAssembler,
    ) -> Self {
        tables.traps.clear();
        (tables.traps).resize((sites.inline_frames.len() + 1) * Trap::COUNT, None);
        tables.thunks.clear();
        tables.thunk_of.clear();
        tables.barriers.clear();
        tables.returns.clear();
        tables.direct_calls.clear();
        Self {
            env,
            code,
            takes_over_frame,
            sites,
            tables,
            entry: masm.new_label(),
        }
    }

    pub(super) fn finish(self, code: Code, compiled: &Compiled) -> CompiledFunction {
        let call_site = |site: &Return| CallSite {
            return_offset: code.offset_of(site.address),
            handler_offset: (site.handler).map_or(u32::MAX, |handler| code.offset_of(handler)),
            inline_frame: Sites::inline_frame(site.origin),
        };
        let direct_call = |&(end, callee): &(Label, CodeIndex)| DirectCall {
            end_offset: code.offset_of(end),
            callee,
        };
        let range = |&(start, origin): &(Label, Origin)| {
            (code.offset_of(start), Sites::inline_frame(origin))
        };
        CompiledFunction {
            entry_offset: code.offset_of(self.entry),
            frame_size: compiled.frame.size,
            call_sites: self.tables.returns.iter().map(call_site).collect(),
            inline_frames: self.sites.inline_frames.clone(),
            inline_ranges: compiled.emitted.origins.iter().map(range).collect(),
            direct_calls: self.tables.direct_calls.iter().map(direct_call).collect(),
            code,
        }
    }

    fn trap_label(&mut self, masm: &mut MacroAssembler, token: Token) -> Label {
        *self.tables.traps[token.0 as usize].get_or_insert_with(|| masm.new_label())
    }

    fn note_return(&mut self, address: Label, handler: Option<Label>, origin: Origin) {
        if handler.is_some() || self.sites.has_inline_frames() {
            self.tables.returns.push(Return {
                address,
                handler,
                origin,
            });
        }
    }

    fn call_trap(&mut self, masm: &mut MacroAssembler, trap: Trap, origin: Origin) {
        masm.call_address(self.env.stubs.traps[trap as usize]);
        let address = masm.mark();
        self.note_return(address, None, origin);
    }

    fn load_memory_base(&self, masm: &mut MacroAssembler) {
        let offset = InstanceLayout::memory_base(self.env.host);
        masm.load64(MEMORY_BASE, Address::new(INSTANCE, offset));
    }

    /// Sets up the registers that the callee finds, other than arguments. Returns where a direct call goes.
    fn prepare_call(
        &mut self,
        masm: &mut MacroAssembler,
        callee: Callee,
        site: &PatchpointSite<'_>,
    ) -> Option<CodeIndex> {
        const _: () = assert!(Callable::SLOT + 8 == Callable::INSTANCE);
        match callee {
            Callee::Code(code) => Some(code),
            Callee::Callable { offset } => {
                let base = site.operands[site.operands.len() - 1].gpr();
                masm.load_pair64(SLOT, INSTANCE, Address::new(base, offset + Callable::SLOT));
                self.load_memory_base(masm);
                None
            }
        }
    }

    fn thunk(&mut self, masm: &mut MacroAssembler, callee: CodeIndex) -> Label {
        let Tables {
            thunks, thunk_of, ..
        } = self.tables;
        thunk_of.get_or_insert_with(callee.0.into(), || {
            let label = masm.new_label();
            thunks.push((callee, label));
            label
        })
    }

    fn call(&mut self, masm: &mut MacroAssembler, callee: Callee, site: &PatchpointSite<'_>) {
        match self.prepare_call(masm, callee, site) {
            None => masm.call_indirect(Address::new(SLOT, FunctionSlot::ENTRY)),
            Some(code) if code == self.code && !self.takes_over_frame => masm.call(self.entry),
            Some(code) => {
                let thunk = self.thunk(masm, code);
                let end = masm.patchable_call(thunk);
                self.tables.direct_calls.push((end, code));
            }
        }
    }

    fn tail_call(
        &mut self,
        masm: &mut MacroAssembler,
        callee: Callee,
        ty: TypeIndex,
        site: &PatchpointSite<'_>,
    ) {
        let (module, direct) = (self.env.module, self.prepare_call(masm, callee, site));
        masm.set_scratch(USUAL_SCRATCH);
        tail_call::leave_frame(
            masm,
            &Signature::new(module.func_type(module.func_index(self.code))),
            &Signature::new(module.types().func_type(ty)),
        );
        match direct {
            None => masm.jump_indirect(Address::new(SLOT, FunctionSlot::ENTRY)),
            Some(code) => {
                let thunk = self.thunk(masm, code);
                let end = masm.patchable_jump(thunk);
                self.tables.direct_calls.push((end, code));
            }
        }
    }

    fn write_barrier(&mut self, masm: &mut MacroAssembler, site: &PatchpointSite<'_>) {
        let (cell, threshold) = (site.operands[0].gpr(), site.operands[1].gpr());
        let (state, start) = (site.scratch_gprs[0], masm.new_label());
        masm.load8(state, Address::new(cell, self.env.host.cell_state));
        masm.branch32(Cond::BelowEq, state, threshold, start);
        self.tables.barriers.push(Barrier {
            start,
            end: masm.here(),
            cell,
            live_registers: site.live_registers,
            live_vectors: site.live_vectors,
        });
    }

    fn call_barrier(&self, masm: &mut MacroAssembler, barrier: &Barrier) {
        let lost_vectors = barrier.live_vectors & abi::CALLEE_SAVED_LOW_HALF;
        let saved = barrier.live_registers & abi::CALLER_SAVED | lost_vectors;
        let count = |bank| saved.in_bank(bank).len();
        let size = (count(Bank::Gpr) * 8 + count(Bank::Fpr) * 16).next_multiple_of(16);
        let (stack_pointer, vector_bytes) = (abi::STACK_POINTER, 16);
        masm.bind(barrier.start);
        masm.sub64(stack_pointer, stack_pointer, size);
        masm.store_registers(saved, vector_bytes, Address::new(stack_pointer, 0));
        masm.move64(abi::ARGUMENT_GPRS[1], barrier.cell);
        masm.move64(abi::ARGUMENT_GPRS[0], INSTANCE);
        masm.call_address(self.env.stubs.operations[Operation::WriteBarrier as usize]);
        masm.load_registers(saved, vector_bytes, Address::new(stack_pointer, 0));
        masm.add64(stack_pointer, stack_pointer, size);
        masm.jump(barrier.end);
    }
}

impl Client<MacroAssembler> for Emitter<'_> {
    fn prologue(&mut self, masm: &mut MacroAssembler, _: usize, frame: &Frame) {
        let (stack_pointer, bottom) = (abi::STACK_POINTER, in_frame(-(frame.size as i32)));
        masm.bind(self.entry);
        if self.takes_over_frame {
            return masm.compute_address(stack_pointer, bottom);
        }
        masm.enter_frame();
        let callee = callee_word(self.env.module.func_index(self.code));
        if cfg!(target_arch = "x86_64") {
            masm.store64(in_frame(frame::INSTANCE), INSTANCE);
            masm.store64(in_frame(frame::CALLEE), callee);
        } else {
            masm.move32(SLOT, callee);
            masm.store_pair64(in_frame(frame::INSTANCE), INSTANCE, SLOT);
        }
        let limit = Address::new(INSTANCE, InstanceLayout::stack_limit(self.env.host));
        let overflow = Sites::exit_token(Trap::StackOverflow, Origin::default());
        if frame.size > UNCHECKED_STACK_EXTENSION {
            let overflow = self.trap_label(masm, overflow);
            masm.compute_address(SLOT, bottom);
            masm.branch64(Cond::Below, SLOT, limit, overflow);
            return masm.move64(stack_pointer, SLOT);
        }
        if frame.size != 0 {
            masm.sub64(stack_pointer, stack_pointer, frame.size);
        }
        if self.sites.makes_calls || frame.size > UNCHECKED_FRAME_SIZE {
            let overflow = self.trap_label(masm, overflow);
            masm.branch64(Cond::Below, stack_pointer, limit, overflow);
        }
    }

    fn shared_exit(&mut self, masm: &mut MacroAssembler, token: Token) -> Option<Label> {
        Some(self.trap_label(masm, token))
    }

    fn exit(&mut self, _: &mut MacroAssembler, _: &ExitSite<'_>) {
        unreachable!("all exits are shared")
    }

    fn patchpoint(&mut self, masm: &mut MacroAssembler, site: &PatchpointSite<'_>) {
        match self.sites.sites[site.token.0 as usize] {
            Site::Nothing => {}
            Site::Trap(trap) => self.call_trap(masm, trap, site.origin),
            Site::Truncate { from, sign, to } => {
                let token = Sites::exit_token(Trap::TruncationOutOfRange, site.origin);
                let failure = self.trap_label(masm, token);
                let (dst, src) = (site.results[0].gpr(), site.operands[0].fpr());
                if let [temp] = *site.scratch_gprs {
                    let gprs = [Some(temp), None];
                    masm.set_scratch(Scratch {
                        gprs,
                        ..masm.scratch()
                    });
                }
                masm.truncate_float_or_branch(from, sign, to, dst, src, failure);
            }
            Site::WriteBarrier => self.write_barrier(masm, site),
            Site::Publish => {
                let done = masm.new_label();
                masm.branch_if_zero32(site.operands[0].gpr(), done);
                masm.store_fence();
                masm.bind(done);
            }
            Site::Call { callee, .. } => self.call(masm, callee, site),
            Site::TailCall(callee, ty) => self.tail_call(masm, callee, ty, site),
        }
    }

    fn after_call(&mut self, masm: &mut MacroAssembler, site: &CallEnd<'_>) {
        let handler = site.handlers.first().copied();
        self.note_return(site.return_address, handler, site.origin);
        if let Site::Call {
            may_leave_instance: true,
            ..
        } = self.sites.sites[site.token.0 as usize]
        {
            masm.load64(INSTANCE, in_frame(frame::INSTANCE));
            if pins_memory_base(self.env) {
                self.load_memory_base(masm);
            }
        }
    }

    fn out_of_line(&mut self, masm: &mut MacroAssembler) {
        for barrier in &self.tables.barriers {
            self.call_barrier(masm, barrier);
        }
        for &(callee, label) in &self.tables.thunks {
            masm.bind(label);
            masm.move64(SLOT, self.env.slots + callee.usize() * FunctionSlot::SIZE);
            masm.jump_indirect(Address::new(SLOT, FunctionSlot::ENTRY));
        }
        for token in 0..self.tables.traps.len() {
            if let Some(label) = self.tables.traps[token] {
                masm.bind(label);
                let origin = Origin((token / Trap::COUNT) as u32);
                self.call_trap(masm, Trap::ALL[token % Trap::COUNT], origin);
            }
        }
    }
}
