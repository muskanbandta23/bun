//! Which locals hold a value that is yet to be read.

use crate::code::{Body, TooLarge};
use crate::effects::{EdgeKind, Resumption, Run, Written};
use crate::graph::{BlockIndex, Graph, refill};
use crate::instruction::{Opcode, RawInstruction};
use crate::operand::Register;

const MAX_WORDS: usize = 1 << 25;
const EDGE_KINDS: usize = 3;

/// Calls `visit` with the index and the bits of each word of a set that has locals of the run.
#[inline(always)]
fn for_each_word(run: Run, mut visit: impl FnMut(usize, u64)) {
    if run.count == 1 {
        if let Some(local) = run.first.local_index() {
            visit(local as usize / 64, 1 << (local % 64));
        }
        return;
    }
    let first = -1 - i64::from(run.first.offset());
    let (mut local, end) = (first.max(0), first + i64::from(run.count));
    while local < end {
        let next = end.min((local | 63) + 1);
        let bits = u64::MAX >> (64 - (next - local)) << (local % 64);
        visit(local as usize / 64, bits);
        local = next;
    }
}

fn set_bits(mut bits: u64) -> impl Iterator<Item = u32> {
    core::iter::from_fn(move || {
        let bit = (bits != 0).then(|| bits.trailing_zeros())?;
        bits &= bits - 1;
        Some(bit)
    })
}

/// What a block does to the 64 locals of one word.
#[derive(Clone, Copy, Default)]
struct Transfer {
    /// To begin with, what it reads before it writes it.
    live_in: u64,
    /// What it has written when it is left by each kind of edge, at the earliest.
    written: [u64; EDGE_KINDS],
}

#[derive(Clone, Copy)]
struct Summary {
    word: u32,
    block: u32,
    transfer: Transfer,
}

/// Solves for one word of the sets at a time, and visits only the blocks where something of it is live.
#[derive(Default)]
struct Solver {
    summaries: Vec<Summary>,
    /// For each word while it summarizes a block, for each block while it solves for a word.
    transfers: Vec<Transfer>,
    touched: Vec<u32>,
    pending: Vec<u64>,
}

impl Solver {
    fn summarize(
        &mut self,
        body: &Body,
        graph: &Graph,
        resumption: Resumption,
    ) -> Result<(), TooLarge> {
        let words = body.locals().div_ceil(64) as usize;
        refill(&mut self.transfers, words, Transfer::default())?;
        self.summaries.clear();
        self.touched.clear();
        let (transfers, touched) = (self.transfers.as_mut_slice(), &mut self.touched);
        for block in graph.blocks() {
            let mut before_throw = u64::MAX;
            for (_, instruction) in graph.instructions(body, block) {
                instruction.for_each_use_with(body, resumption, |run| {
                    for_each_word(run, |word, bits| {
                        let transfer = &mut transfers[word];
                        if transfer.is_untouched() {
                            touched.push(word as u32);
                        }
                        // What is written on one way out only is written by the last instruction.
                        transfer.live_in |= bits & !transfer.written[EdgeKind::Jump as usize];
                    });
                });
                if instruction.opcode.flow().can_throw() {
                    before_throw = 0;
                }
                instruction.for_each_def_with(body, resumption, |run, written| {
                    for_each_word(run, |word, bits| {
                        let transfer = &mut transfers[word];
                        if transfer.is_untouched() {
                            touched.push(word as u32);
                        }
                        let [falling, jumping, throwing] = &mut transfer.written;
                        match written {
                            Written::Always => {
                                *falling |= bits;
                                *jumping |= bits;
                                *throwing |= bits & before_throw;
                            }
                            Written::IfJumps => *jumping |= bits,
                            Written::UnlessJumps => *falling |= bits,
                        }
                    });
                });
            }
            if self.summaries.len() + touched.len() > MAX_WORDS
                || self.summaries.try_reserve(touched.len()).is_err()
            {
                return Err(TooLarge);
            }
            self.summaries.extend(touched.drain(..).map(|word| Summary {
                word,
                block: block.0,
                transfer: core::mem::take(&mut transfers[word as usize]),
            }));
        }
        if words > 1 {
            self.summaries.sort_unstable_by_key(|summary| summary.word);
        }
        Ok(())
    }

    /// Calls `visit` for each word in ascending order with the blocks at whose start something of it is live.
    fn solve(
        &mut self,
        body: &Body,
        graph: &Graph,
        resumption: Resumption,
        mut visit: impl FnMut(u32, &[u32], &[Transfer]) -> Result<(), TooLarge>,
    ) -> Result<(), TooLarge> {
        self.summarize(body, graph, resumption)?;
        refill(&mut self.transfers, graph.len(), Transfer::default())?;
        refill(&mut self.pending, graph.len() / 64 + 1, 0)?;
        let summaries = core::mem::take(&mut self.summaries);
        for summaries in summaries.chunk_by(|a, b| a.word == b.word) {
            self.touched.clear();
            for summary in summaries {
                self.transfers[summary.block as usize] = summary.transfer;
                if summary.transfer.live_in != 0 {
                    self.touched.push(summary.block);
                    self.pending[summary.block as usize / 64] |= 1 << (summary.block % 64);
                }
            }
            self.propagate(graph);
            visit(summaries[0].word, &self.touched, &self.transfers)?;
            for &block in &self.touched {
                self.transfers[block as usize].live_in = 0;
            }
            for summary in summaries {
                self.transfers[summary.block as usize] = Transfer::default();
            }
        }
        self.summaries = summaries;
        Ok(())
    }

    /// From the end of the code to its start, and again for as long as loops lead back up.
    fn propagate(&mut self, graph: &Graph) {
        let mut remaining = self.touched.len();
        let mut highest = self.pending.len() - 1;
        while remaining != 0 {
            let mut word = core::mem::take(&mut highest);
            loop {
                let Some(bit) = self.pending[word].highest_one() else {
                    if word == 0 {
                        break;
                    }
                    word -= 1;
                    continue;
                };
                self.pending[word] ^= 1 << bit;
                remaining -= 1;
                let block = word as u32 * 64 + bit;
                let live = self.transfers[block as usize].live_in;
                for edge in graph.predecessors(BlockIndex(block)) {
                    let from = edge.block.index();
                    let transfer = &mut self.transfers[from];
                    let new = live & !transfer.written[edge.kind as usize] & !transfer.live_in;
                    if new == 0 {
                        continue;
                    }
                    if transfer.live_in == 0 {
                        self.touched.push(from as u32);
                    }
                    transfer.live_in |= new;
                    let (pending, bit) = (&mut self.pending[from / 64], 1 << (from % 64));
                    remaining += usize::from(*pending & bit == 0);
                    *pending |= bit;
                    highest = highest.max(from / 64);
                }
                if remaining == 0 {
                    break;
                }
            }
        }
    }
}

impl Transfer {
    #[inline(always)]
    fn is_untouched(&self) -> bool {
        let [falling, jumping, _] = self.written;
        self.live_in | falling | jumping == 0
    }
}

/// A set of locals.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Locals<'a>(&'a [u64]);

impl<'a> Locals<'a> {
    #[inline]
    pub fn contains(self, register: Register) -> bool {
        register.local_index().is_some_and(|local| {
            let word = self.0.get(local as usize / 64);
            word.is_some_and(|word| word >> (local % 64) & 1 != 0)
        })
    }

    pub fn is_empty(self) -> bool {
        self.0.iter().all(|&word| word == 0)
    }

    /// From local 0 on.
    pub fn iter(self) -> impl Iterator<Item = Register> + 'a {
        (0..).zip(self.0).flat_map(|(word, &bits)| {
            set_bits(bits).map(move |bit| Register::local(word * 64 + bit))
        })
    }

    /// Local `n` is bit `n % 64` of word `n / 64`.
    #[inline]
    pub fn words(self) -> &'a [u64] {
        self.0
    }
}

impl core::fmt::Debug for Locals<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_set().entries(self.iter()).finish()
    }
}

pub struct Liveness {
    graph: Graph,
    words: usize,
    live_in: Box<[u64]>,
}

/// Room for [`Liveness::walk_backwards`] to work in.
#[derive(Default)]
pub struct Walk {
    live: Vec<u64>,
    offsets: Vec<u32>,
}

impl Liveness {
    /// Of verified code.
    pub fn new(body: &Body) -> Result<Liveness, TooLarge> {
        let graph = Graph::new(body)?;
        let words = body.locals().div_ceil(64) as usize;
        let len = graph.len().saturating_mul(words);
        if len > MAX_WORDS {
            return Err(TooLarge);
        }
        let mut live_in = Vec::new();
        refill(&mut live_in, len, 0)?;
        let keep = |word: u32, blocks: &[u32], transfers: &[Transfer]| {
            for &block in blocks {
                live_in[block as usize * words + word as usize] = transfers[block as usize].live_in;
            }
            Ok(())
        };
        Solver::default().solve(body, &graph, Resumption::Dispatched, keep)?;
        Ok(Liveness {
            graph,
            words,
            live_in: live_in.into(),
        })
    }

    #[inline]
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    #[inline]
    pub fn live_in(&self, block: BlockIndex) -> Locals<'_> {
        Locals(&self.live_in[block.index() * self.words..][..self.words])
    }

    /// Calls `visit` for each instruction of the block, the last first, with what is live before it.
    pub fn walk_backwards(
        &self,
        body: &Body,
        block: BlockIndex,
        walk: &mut Walk,
        visit: impl FnMut(u32, &RawInstruction, Locals<'_>),
    ) {
        self.walk_back_to(self.graph.range(block).start, body, walk, visit);
    }

    /// Takes time in proportion to the rest of the block.
    pub fn live_before<'a>(&self, body: &Body, offset: u32, walk: &'a mut Walk) -> Locals<'a> {
        self.walk_back_to(offset, body, walk, |_, _, _| {});
        Locals(&walk.live)
    }

    fn walk_back_to(
        &self,
        start: u32,
        body: &Body,
        walk: &mut Walk,
        mut visit: impl FnMut(u32, &RawInstruction, Locals<'_>),
    ) {
        let block = self.graph.block_at(start);
        let (code, end) = (body.instructions(), self.graph.range(block).end);
        let Walk { live, offsets } = walk;
        offsets.clear();
        let mut offset = start;
        while offset < end {
            offsets.push(offset);
            offset += Opcode::read(code, offset).expect("verified").1;
        }
        live.clear();
        live.resize(self.words, 0);
        let handler = self.graph.handler(block);
        let mut is_last = true;
        for &offset in offsets.iter().rev() {
            let instruction = RawInstruction::read(code, offset).expect("verified");
            if core::mem::take(&mut is_last) {
                self.add_successors(body, block, &instruction, live);
            } else {
                instruction.for_each_def(body, |run, _| {
                    for_each_word(run, |word, bits| live[word] &= !bits);
                });
            }
            if let Some(handler) = handler
                && instruction.opcode.flow().can_throw()
            {
                self.add_live_in(handler, live);
            }
            instruction.for_each_use(body, |run| {
                for_each_word(run, |word, bits| live[word] |= bits);
            });
            visit(offset, &instruction, Locals(live));
        }
    }

    fn add_live_in(&self, block: BlockIndex, live: &mut [u64]) {
        for (live, &more) in live.iter_mut().zip(self.live_in(block).0) {
            *live |= more;
        }
    }

    /// What is live after the last instruction of the block and not written by it.
    fn add_successors(
        &self,
        body: &Body,
        block: BlockIndex,
        last: &RawInstruction,
        live: &mut [u64],
    ) {
        let edges = self.graph.successors(block).iter();
        let edges = edges.filter(|edge| edge.kind != EdgeKind::Throw);
        for edge in edges.clone() {
            self.add_live_in(edge.block, live);
        }
        last.for_each_def(body, |run, written| {
            for_each_word(run, |word, bits| {
                live[word] &= !bits;
                if written != Written::Always {
                    for edge in edges.clone().filter(|edge| !written.holds_on(edge.kind)) {
                        live[word] |= self.live_in(edge.block).0[word] & bits;
                    }
                }
            });
        });
    }
}

/// The locals of one word that a suspend point has to save.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Saved {
    point: u32,
    word: u32,
    bits: u64,
}

#[derive(Default)]
pub(crate) struct SavedRegisters {
    graph: Graph,
    solver: Solver,
    saved: Vec<Saved>,
    registers: Vec<Register>,
}

impl SavedRegisters {
    pub(crate) fn fill_in(&mut self, body: &mut Body) -> Result<(), TooLarge> {
        let SavedRegisters {
            graph,
            solver,
            saved,
            registers,
        } = self;
        graph.build(body, Resumption::Direct)?;
        saved.clear();
        solver.solve(body, graph, Resumption::Direct, |word, _, transfers| {
            for (point, state) in (0..).zip(body.suspend_points()) {
                let live = transfers[graph.block_at(state.resume).index()].live_in;
                // `enter` loads the scope of the callee on a resumption too.
                let bits = if word == 0 { live & !1 } else { live };
                if bits != 0 {
                    saved.push(Saved { point, word, bits });
                }
            }
            (saved.len() <= MAX_WORDS).ok_or(TooLarge)
        })?;
        saved.sort_unstable();

        registers.clear();
        let tables = body.tables.as_mut().expect("has suspend points");
        let (mut previous, mut range): (&[Saved], _) = (&[], 0..0);
        for saved in saved.chunk_by(|a, b| a.point == b.point) {
            let locals = |saved: &Saved| (saved.word, saved.bits);
            if !saved.iter().map(locals).eq(previous.iter().map(locals)) {
                range.start = range.end;
                for &Saved { word, bits, .. } in saved {
                    if registers.len() > MAX_WORDS || registers.try_reserve(64).is_err() {
                        return Err(TooLarge);
                    }
                    registers.extend(set_bits(bits).map(|bit| Register::local(word * 64 + bit)));
                }
                range.end = registers.len() as u32;
                previous = saved;
            }
            tables.suspend_points[saved[0].point as usize].registers = range.clone();
        }
        tables.saved_registers = registers.as_slice().into();
        Ok(())
    }
}
