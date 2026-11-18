//! Calls, and functions that are translated in place of a call.

use bun_engine_bytecode::{CodeFlags, MetadataKind, Register};
use bun_engine_codegen::ir::{State, Value as Ssa};
use bun_engine_vm::code::metadata::{Call, SuperConstruct};
use bun_engine_vm::ic::CallProfile;
use bun_engine_vm::{
    CodeBlock, Executable, FunctionExecutable, Gc, JsFunction, SpeculatedType as T,
};
use smallvec::SmallVec;

use super::frame::{ArgumentCount, Frame, Junction};
use super::{Builder, Built};
use crate::graph::{CallKind, ExitKind, Kind};

const MAX_INLINING_DEPTH: usize = 5;
const MAX_INLINING_RECURSION: usize = 2;

pub(super) struct CallSite<'a> {
    pub(super) kind: CallKind,
    pub(super) dst: Register,
    pub(super) callee: Ssa,
    /// `this` or `new.target`, and the arguments.
    pub(super) arguments: &'a [Ssa],
    pub(super) metadata_kind: MetadataKind,
    pub(super) metadata: u32,
}

impl Builder<'_> {
    pub(super) fn outgoing_arguments(
        &self,
        this: Ssa,
        frame: u32,
        count: u32,
    ) -> SmallVec<[Ssa; 8]> {
        let arguments =
            (0..count - 1).map(|index| self.frame().get(Register::outgoing_argument(frame, index)));
        core::iter::once(this).chain(arguments).collect()
    }

    /// Checks that the callee is what has been called so far, if that is one thing.
    fn known_executable(
        &mut self,
        callee: &mut Ssa,
        profile: &CallProfile,
    ) -> Option<Gc<Executable>> {
        if let Some(function) = self.constant_cell::<JsFunction>(*callee) {
            return Some(function.executable());
        }
        let root = self.graph.root(*callee);
        if self.graph.kind(root) == Some(Kind::NewFunction) {
            return self
                .constant_cell::<FunctionExecutable>(self.graph.operand(root, 1))
                .map(Gc::upcast);
        }
        let only_callee = profile.callee().get().and_then(Gc::downcast::<JsFunction>);
        if let Some(function) = only_callee.filter(|_| !self.has_exited(ExitKind::BadIdentity)) {
            let expected = self.cell(function.cell());
            self.check_identical(*callee, expected);
            *callee = expected;
            return Some(function.executable());
        }
        let executable = profile
            .executable()
            .get()
            .and_then(Gc::downcast::<FunctionExecutable>)?;
        if self.has_exited(ExitKind::BadExecutable) || self.has_exited(ExitKind::BadType) {
            return None;
        }
        let operands = [
            self.check_type(*callee, T::FUNCTION),
            self.cell(executable.cell()),
        ];
        *callee = self.node(Kind::CheckExecutable, &operands, 0);
        let function_type = T::FUNCTION;
        self.knowledge
            .learn_type(&self.graph, *callee, function_type, *callee);
        Some(executable.upcast())
    }

    pub(super) fn call(&mut self, mut site: CallSite<'_>) -> Built {
        let code_block = self.frame().code_block;
        let profile = match site.metadata_kind {
            MetadataKind::Call => {
                &code_block
                    .checked_metadata::<Call>(site.metadata_kind, site.metadata)
                    .profile
            }
            _ => {
                &code_block
                    .checked_metadata::<SuperConstruct>(site.metadata_kind, site.metadata)
                    .call
                    .profile
            }
        };
        if !profile.has_run() && !self.has_exited(ExitKind::NeverExecuted) {
            self.force_exit(ExitKind::NeverExecuted);
            return Ok(());
        }
        let executable = self.known_executable(&mut site.callee, profile);
        let script = executable.and_then(Gc::downcast::<FunctionExecutable>);
        if let Some(target) = script.and_then(|script| self.inlinable_code(script, &site)) {
            return self.inline(&site, target);
        }
        let is_real_tail_call = site.kind == CallKind::TailCall
            && self
                .frames
                .iter()
                .all(|frame| self.is_in_tail_position(frame));
        let prediction = if is_real_tail_call {
            Some(T::empty())
        } else {
            self.prediction_or_exit(site.metadata)
        };
        let Some(prediction) = prediction else {
            return Ok(());
        };

        let mut operands: SmallVec<[Ssa; 12]> = SmallVec::new();
        let direct = script
            .and_then(|script| Some((script, script.code_block(site.kind.specialization())?)));
        operands.extend(direct.map(|(script, _)| self.cell(script.cell())));
        operands.extend(
            core::iter::once(&site.callee)
                .chain(site.arguments)
                .map(|&operand| self.graph.root(operand)),
        );
        if let Some((_, target)) = direct {
            let undefined = self.undefined();
            operands.resize(
                operands.len().max(2 + target.parameters() as usize),
                undefined,
            );
        }
        let kind = match (site.kind, is_real_tail_call, direct.is_some()) {
            (_, true, false) => Kind::TailCall,
            (_, true, true) => Kind::DirectTailCall,
            (CallKind::Construct, _, false) => Kind::Construct,
            (CallKind::Construct, _, true) => Kind::DirectConstruct,
            (_, _, false) => Kind::Call,
            (_, _, true) => Kind::DirectCall,
        };
        let result = self.profiled_node(kind, &operands, site.arguments.len() as u32, prediction);
        if is_real_tail_call {
            self.end_block();
        } else {
            self.set(site.dst, result);
        }
        Ok(())
    }

    fn is_in_tail_position(&self, frame: &Frame) -> bool {
        frame.is_root() || self.graph.frames[frame.index.index()].kind == CallKind::TailCall
    }

    fn inlinable_code(
        &self,
        script: Gc<FunctionExecutable>,
        site: &CallSite<'_>,
    ) -> Option<Gc<CodeBlock>> {
        let target = script.code_block(site.kind.specialization())?.baseline();
        let size = target.body().instructions().len() as u32;
        let (max_size, budget) = if self.is_full() {
            (400, 24_000)
        } else {
            (200, 5000)
        };
        let recursion = self
            .frames
            .iter()
            .filter(|frame| frame.code_block == target)
            .count();
        let unsuitable = CodeFlags::SUSPENDS | CodeFlags::HAS_DEBUG_HOOKS;
        let is_suitable = size <= max_size
            && self.inlined_size + size <= budget
            && self.frames.len() <= MAX_INLINING_DEPTH
            && recursion < MAX_INLINING_RECURSION
            && !script.never_inline().get()
            && !target.body().flags().intersects(unsuitable);
        is_suitable.then_some(target)
    }

    fn inline(&mut self, site: &CallSite<'_>, target: Gc<CodeBlock>) -> Built {
        self.inlined_size += target.body().instructions().len() as u32;
        let caller_layout = self.describe_waiting_caller(Some(site.dst));
        let info = self.describe_frame(target, site.kind);
        let count = site.arguments.len() as u32;
        let mut frame = Frame::new(
            &mut self.graph,
            info,
            target,
            count.max(info.parameters),
            ArgumentCount::Known(count),
        )?;
        (frame.caller_layout, frame.caller_has_handler) =
            (caller_layout, self.frame().has_handler());
        frame.caller_values.clone_from(&self.scratch);
        frame.set(Register::CALLEE, site.callee);
        let undefined = self.undefined();
        for (position, register) in frame.argument_registers().enumerate() {
            frame.set(
                register,
                site.arguments.get(position).copied().unwrap_or(undefined),
            );
        }
        frame.remember_arguments();
        for (position, &argument) in site
            .arguments
            .iter()
            .enumerate()
            .take(info.parameters as usize)
        {
            let root = self.graph.root(argument);
            if self.graph.prediction(root).is_empty() {
                self.graph
                    .predict(root, target.argument_prediction(position as u32).get());
            }
        }

        let origin = self.origin;
        self.frames.push(frame);
        self.build_blocks(bun_engine_bytecode::BlockIndex(0))?;
        let mut frame = self.frames.pop().unwrap();
        self.origin = origin;
        let result = match frame.returned_in_place {
            Some((block, result)) => {
                self.block = block;
                Some(result)
            }
            None => self.enter(&core::mem::take(&mut frame.continuation)),
        };
        if let Some(result) = result {
            // The state from before the call is as good as it was if the function did nothing but compute.
            self.has_had_effect = self.checkpoint.is_none();
            self.set(site.dst, result);
        }
        Ok(())
    }

    pub(super) fn leave_with(&mut self, value: Ssa) {
        if self.frame().is_root() {
            if !self.is_full() {
                self.node(Kind::TierUpAtReturn, &[], 0);
            }
            let value = self.graph.root(value);
            self.ir().ret(&[value]);
            self.block = bun_engine_codegen::ir::Block::NONE;
        } else if self.frame().is_last_way_out() {
            let block = core::mem::replace(&mut self.block, bun_engine_codegen::ir::Block::NONE);
            self.frame_mut().returned_in_place = Some((block, value));
        } else {
            let mut continuation = core::mem::take(&mut self.frame_mut().continuation);
            self.leave_for(&mut continuation, Some(value));
            self.frame_mut().continuation = continuation;
        }
    }

    /// Jumps to a junction that carries one value or none.
    pub(super) fn leave_for(&mut self, junction: &mut Junction, value: Option<Ssa>) {
        let block = *junction
            .block
            .get_or_insert_with(|| self.graph.function.new_block());
        self.ir().jump(block, &[]);
        self.scratch.clear();
        self.scratch
            .extend(value.map(|value| self.graph.root(value)));
        self.join(junction, self.graph.function.successor(self.block, 0));
        self.block = bun_engine_codegen::ir::Block::NONE;
    }

    /// Gives the value, which is `NONE` if the junction carries none, or nothing if no way leads there.
    pub(super) fn enter(&mut self, junction: &Junction) -> Option<Ssa> {
        if junction.edges.is_empty() {
            return None;
        }
        self.block = junction.block.unwrap();
        self.knowledge.restore(&self.graph, &junction.knowledge);
        self.needs_invalidation_point = junction.follows_effect;
        self.checkpoint = State::NONE;
        Some(junction.values.first().copied().unwrap_or(Ssa::NONE))
    }
}

impl Frame {
    /// Nothing has returned yet, and nothing is waiting to be translated.
    fn is_last_way_out(&self) -> bool {
        self.continuation.edges.is_empty()
            && self
                .junctions
                .iter()
                .all(|junction| junction.is_entered || junction.edges.is_empty())
    }
}
