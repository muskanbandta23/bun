//! The state of the translation of one function.

use std::rc::Rc;

use bun_engine_bytecode::{BlockIndex, Liveness, Locals, Register};
use bun_engine_codegen::ir::{Block, Edge, Value as Ssa};
use bun_engine_vm::{CodeBlock, Gc};
use smallvec::SmallVec;

use super::{Built, Refusal};
use crate::graph::{FrameIndex, FrameInfo, Graph, NO_LAYOUT, SavedKnowledge};

#[derive(Clone, Copy)]
pub(super) enum ArgumentCount {
    InFrame,
    /// Including `this`.
    Known(u32),
}

/// A place where several ways meet.
#[derive(Default)]
pub(super) struct Junction {
    pub(super) block: Option<Block>,
    pub(super) edges: SmallVec<[Edge; 2]>,
    /// For each register that is live, in the order of `Frame::carried`.
    pub(super) values: Vec<Ssa>,
    /// For each parameter of the block, its position in `values`.
    pub(super) params: Vec<u32>,
    pub(super) knowledge: SavedKnowledge,
    pub(super) is_entered: bool,
    /// No more edges will leave the block.
    pub(super) is_done: bool,
    /// On some way here, assumptions may have failed since the last invalidation point.
    pub(super) follows_effect: bool,
}

pub(super) struct Frame {
    pub(super) index: FrameIndex,
    pub(super) code_block: Gc<CodeBlock>,
    pub(super) liveness: Rc<Liveness>,
    pub(super) argument_count: ArgumentCount,
    /// From the last local up to the last argument.
    registers: Vec<Ssa>,
    locals: u32,
    /// What the slots from the callee up held at the start.
    at_entry: Vec<Ssa>,
    pub(super) junctions: Vec<Junction>,
    /// Where the function returns to, if it is inlined.
    pub(super) continuation: Junction,
    pub(super) returned_in_place: Option<(Block, Ssa)>,
    /// What a state has for the callers.
    pub(super) caller_layout: u32,
    pub(super) caller_values: Vec<Ssa>,
    pub(super) caller_has_handler: bool,
    /// For each instruction of the current block, last first, the locals that are live before it.
    pub(super) live_before: Vec<u64>,
    pub(super) words: usize,
    pub(super) offset: u32,
    pub(super) current: BlockIndex,
}

impl Frame {
    /// `slots` is the number of arguments that have a register, including `this`.
    pub(super) fn new(
        graph: &mut Graph,
        info: FrameInfo,
        code_block: Gc<CodeBlock>,
        slots: u32,
        argument_count: ArgumentCount,
    ) -> Built<Self> {
        let liveness = code_block.liveness().map_err(|_| Refusal::TooLarge)?;
        graph.frames.push(info);
        let locals = code_block.locals();
        Ok(Self {
            index: FrameIndex(graph.frames.len() as u32 - 1),
            code_block,
            argument_count,
            registers: vec![Ssa::NONE; (locals + Register::HEADER_WORDS + slots) as usize],
            locals,
            at_entry: Vec::new(),
            junctions: (0..liveness.graph().len())
                .map(|_| Junction::default())
                .collect(),
            continuation: Junction::default(),
            returned_in_place: None,
            caller_layout: NO_LAYOUT,
            caller_values: Vec::new(),
            caller_has_handler: false,
            live_before: Vec::new(),
            words: locals.div_ceil(64) as usize,
            offset: 0,
            current: BlockIndex(0),
            liveness,
        })
    }

    #[inline]
    fn position(&self, register: Register) -> usize {
        (register.offset() + self.locals as i32) as usize
    }

    #[inline]
    pub(super) fn get(&self, register: Register) -> Ssa {
        self.registers[self.position(register)]
    }

    #[inline]
    pub(super) fn set(&mut self, register: Register, value: Ssa) {
        let position = self.position(register);
        self.registers[position] = value;
    }

    pub(super) fn is_root(&self) -> bool {
        self.index == FrameIndex::ROOT
    }

    /// `this` and the arguments.
    pub(super) fn argument_registers(&self) -> impl Iterator<Item = Register> + use<> {
        let end = self.registers.len() as i32 - self.locals as i32;
        (Register::THIS.offset()..end).map(Register::from_offset)
    }

    /// What an edge carries to a block at whose start `live` is live.
    pub(super) fn carried<'a>(&self, live: Locals<'a>) -> impl Iterator<Item = Register> + use<'a> {
        self.argument_registers().chain(live.iter())
    }

    pub(super) fn remember_arguments(&mut self) {
        self.at_entry = self.registers[self.position(Register::CALLEE)..].to_vec();
    }

    pub(super) fn original_argument(&self, argument: Register) -> Ssa {
        self.at_entry[(argument.offset() - Register::CALLEE.offset()) as usize]
    }

    /// The frame on the stack has the value.
    pub(super) fn is_in_place(&self, argument: Register) -> bool {
        self.is_root() && self.original_argument(argument) == self.get(argument)
    }

    pub(super) fn live_locals(&self) -> impl Iterator<Item = Register> {
        let words = &self.live_before[self.live_before.len() - self.words..];
        words.iter().enumerate().flat_map(|(index, &word)| {
            let bits = core::iter::successors((word != 0).then_some(word), |&rest| {
                Some(rest & (rest - 1)).filter(|&rest| rest != 0)
            });
            bits.map(move |rest| Register::local(index as u32 * 64 + rest.trailing_zeros()))
        })
    }

    pub(super) fn has_handler(&self) -> bool {
        self.caller_has_handler || self.liveness.graph().handler(self.current).is_some()
    }
}
