//! Origins, and the states to exit to.

use bun_engine_bytecode::Register;
use bun_engine_codegen::ir::{Exit, Origin, State};

use super::Builder;
use super::frame::ArgumentCount;
use crate::graph::{ARGUMENT_COUNT, ExitKind, Kind, Layout, NO_RECIPE, Position, Site};

impl Builder<'_> {
    pub(super) fn begin_instruction(&mut self, offset: u32) {
        let frame = self.frames.last_mut().unwrap();
        frame.offset = offset;
        self.origin = Origin(self.graph.sites.len() as u32);
        self.graph.sites.push(Site {
            position: Position {
                frame: frame.index,
                offset,
            },
            exits: frame.code_block.exits_at(offset),
            metadata: 0,
            has_handler: frame.has_handler(),
        });
        self.has_had_effect = false;
        // A handler has to see the registers as they are when the exception is thrown.
        if frame.has_handler() {
            self.checkpoint = State::NONE;
        }
        if !self.graph.function.params(self.block).is_empty()
            && self.graph.function.first(self.block).is_none()
        {
            let state = self.state();
            self.insert_node(Kind::Checkpoint, &[], 0, state);
        }
        if core::mem::take(&mut self.needs_invalidation_point) {
            let state = self.state();
            self.insert_node(Kind::InvalidationPoint, &[], 0, state);
        }
    }

    pub(super) fn has_exited(&self, kind: ExitKind) -> bool {
        self.graph.has_exited(self.origin, kind)
    }

    /// Adds a layout for the innermost frame and leaves the values, with those of the callers, in `scratch`.
    fn describe_registers(&mut self, locals: impl Iterator<Item = Register>) -> u32 {
        let first_register = self.graph.layout_registers.len() as u32;
        let frame = self.frames.last().unwrap();
        self.scratch.clone_from(&frame.caller_values);
        if let ArgumentCount::Known(count) = frame.argument_count {
            self.graph
                .layout_registers
                .extend([Register::CALLEE, ARGUMENT_COUNT]);
            self.scratch.extend([
                frame.get(Register::CALLEE),
                self.graph.function.const64(count.into()),
            ]);
        }
        for register in frame
            .argument_registers()
            .filter(|&argument| !frame.is_in_place(argument))
            .chain(locals)
        {
            self.graph.layout_registers.push(register);
            self.scratch.push(self.graph.root(frame.get(register)));
        }
        self.graph.layouts.push(Layout {
            resume: self.origin,
            caller: frame.caller_layout,
            first_register,
            register_count: self.graph.layout_registers.len() as u32 - first_register,
            recipe: NO_RECIPE,
        });
        self.graph.layouts.len() as u32 - 1
    }

    /// From where all that has been translated since can be done again.
    pub(super) fn state(&mut self) -> State {
        debug_assert!(!self.has_had_effect, "there is no way back from here");
        if self.checkpoint.is_none() {
            let locals: smallvec::SmallVec<[Register; 32]> = self.frame().live_locals().collect();
            let layout = self.describe_registers(locals.into_iter());
            self.checkpoint = self
                .graph
                .function
                .add_described_state(self.scratch.iter().copied(), layout);
        }
        self.checkpoint
    }

    /// For a function that is about to be inlined. `next` is the offset of the instruction after the call, which
    /// writes `result`. The values are left in `scratch`.
    pub(super) fn describe_waiting_caller(&mut self, result: Option<Register>) -> u32 {
        let frame = self.frame();
        let before_this = frame.live_before.len() - frame.words;
        let mut live: smallvec::SmallVec<[u64; 4]> = if before_this == 0 {
            frame.live_at_end().collect()
        } else {
            frame.live_before[before_this - frame.words..before_this].into()
        };
        if let Some(index) = result.and_then(Register::local_index) {
            live[index as usize / 64] &= !(1 << (index % 64));
        }
        if let Some(handler) = frame.liveness.graph().handler(frame.current) {
            live.iter_mut()
                .zip(frame.liveness.live_in(handler).words())
                .for_each(|(live, &more)| *live |= more);
        }
        let locals = (0..live.len() as u32 * 64)
            .filter(move |&index| live[index as usize / 64] >> (index % 64) & 1 != 0);
        self.describe_registers(locals.map(Register::local))
    }

    pub(super) fn exit(&mut self, kind: ExitKind) -> Exit {
        let state = self.state();
        self.graph.exit(kind, self.origin, state)
    }

    pub(super) fn note_effect(&mut self) {
        self.checkpoint = State::NONE;
        self.has_had_effect = true;
    }
}

impl super::frame::Frame {
    fn live_at_end(&self) -> impl Iterator<Item = u64> {
        let successors = self.liveness.graph().successors(self.current);
        (0..self.words).map(move |word| {
            successors.iter().fold(0, |live, edge| {
                live | self.liveness.live_in(edge.block).words()[word]
            })
        })
    }
}
