//! Basic blocks.

use core::ops::Range;

use crate::code::{Body, TooLarge};
use crate::effects::{EdgeKind, Resumption};
use crate::instruction::{Instructions, Opcode, RawInstruction};

/// In the order of the code. The first block is the entry.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct BlockIndex(pub u32);

impl BlockIndex {
    #[inline(always)]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    /// At the other end.
    pub block: BlockIndex,
    pub kind: EdgeKind,
}

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Block {
    start: u32,
    handler: u32,
    first_successor: u32,
    first_predecessor: u32,
}

/// Only the last instruction of a block jumps. Any may throw, all to the same handler.
#[derive(Default)]
pub struct Graph {
    /// A bit for each byte of code, and how many are set in the words before.
    leaders: Vec<(u64, u32)>,
    /// And one that starts where the code ends.
    blocks: Vec<Block>,
    successors: Vec<Edge>,
    predecessors: Vec<Edge>,
    scratch: Vec<u32>,
}

pub(crate) fn refill<T: Copy>(vector: &mut Vec<T>, len: usize, value: T) -> Result<(), TooLarge> {
    vector.clear();
    vector.try_reserve(len).map_err(|_| TooLarge)?;
    vector.resize(len, value);
    Ok(())
}

impl Graph {
    /// Of verified code.
    pub fn new(body: &Body) -> Result<Graph, TooLarge> {
        let mut graph = Graph::default();
        graph.build(body, Resumption::Dispatched)?;
        graph.scratch = Vec::new();
        Ok(graph)
    }

    pub(crate) fn build(&mut self, body: &Body, resumption: Resumption) -> Result<(), TooLarge> {
        self.find_blocks(body, resumption)?;
        self.find_handlers(body)?;
        self.find_successors(body, resumption)?;
        self.find_predecessors()
    }

    fn find_blocks(&mut self, body: &Body, resumption: Resumption) -> Result<(), TooLarge> {
        let code = body.instructions();
        refill(&mut self.leaders, code.len() / 64 + 1, (0, 0))?;
        let leaders = self.leaders.as_mut_slice();
        let mut lead = |offset: u32| leaders[offset as usize / 64].0 |= 1 << (offset % 64);
        lead(0);
        for handler in body.handlers() {
            lead(handler.range.start);
            lead(handler.range.end);
            lead(handler.target);
        }
        let mut offset = 0;
        while let Some((opcode, size)) = Opcode::read(code, offset) {
            if resumption.branches(opcode) {
                let instruction = RawInstruction::read(code, offset).expect("verified");
                instruction.for_each_target(body, offset, &mut lead);
                lead(offset + size);
            } else if !opcode.flow().falls_through() {
                lead(offset + size);
            }
            offset += size;
        }
        lead(offset);

        let mut count = 0;
        for (bits, before) in &mut self.leaders {
            *before = count;
            count += bits.count_ones();
        }
        self.blocks.clear();
        self.blocks
            .try_reserve(count as usize)
            .map_err(|_| TooLarge)?;
        for (word, &(mut bits, _)) in (0..).zip(&self.leaders) {
            while bits != 0 {
                self.blocks.push(Block {
                    start: word * 64 + bits.trailing_zeros(),
                    handler: NONE,
                    first_successor: 0,
                    first_predecessor: 0,
                });
                bits &= bits - 1;
            }
        }
        Ok(())
    }

    fn find_handlers(&mut self, body: &Body) -> Result<(), TooLarge> {
        if body.handlers().is_empty() {
            return Ok(());
        }
        // Leads from a block that has a handler towards the next that has none.
        refill(&mut self.scratch, self.blocks.len(), 0)?;
        for handler in body.handlers() {
            let [mut block, end, target] = [handler.range.start, handler.range.end, handler.target]
                .map(|offset| self.block_at(offset).0);
            while block < end {
                let state = &mut self.blocks[block as usize];
                let skip = &mut self.scratch[block as usize];
                if state.handler == NONE {
                    state.handler = target;
                    *skip = end;
                    block += 1;
                } else {
                    let next = *skip;
                    *skip = next.max(end);
                    block = next;
                }
            }
        }
        Ok(())
    }

    fn find_successors(&mut self, body: &Body, resumption: Resumption) -> Result<(), TooLarge> {
        let code = body.instructions();
        let mut successors = core::mem::take(&mut self.successors);
        successors.clear();
        // The last block that was found to jump to each.
        let mut jumped_from = core::mem::take(&mut self.scratch);
        refill(&mut jumped_from, self.blocks.len(), NONE)?;
        for index in 0..self.len() {
            let block = &mut self.blocks[index];
            block.first_successor = successors.len() as u32;
            let (mut offset, handler) = (block.start, block.handler);
            let end = self.blocks[index + 1].start;
            let mut throws = false;
            let last = loop {
                let (opcode, size) = Opcode::read(code, offset).expect("verified");
                throws |= opcode.flow().can_throw();
                if offset + size >= end {
                    break opcode;
                }
                offset += size;
            };
            if resumption.branches(last) {
                let instruction = RawInstruction::read(code, offset).expect("verified");
                instruction.for_each_target(body, offset, |target| {
                    let block = self.block_at(target);
                    if jumped_from[block.index()] != index as u32 {
                        jumped_from[block.index()] = index as u32;
                        let kind = EdgeKind::Jump;
                        successors.push(Edge { block, kind });
                    }
                });
            }
            if resumption.falls_through(last.flow()) {
                successors.push(Edge {
                    block: BlockIndex(index as u32 + 1),
                    kind: EdgeKind::FallThrough,
                });
            }
            if throws && handler != NONE {
                successors.push(Edge {
                    block: BlockIndex(handler),
                    kind: EdgeKind::Throw,
                });
            }
        }
        let end = self.len();
        self.blocks[end].first_successor = u32::try_from(successors.len()).map_err(|_| TooLarge)?;
        self.successors = successors;
        self.scratch = jumped_from;
        Ok(())
    }

    fn find_predecessors(&mut self) -> Result<(), TooLarge> {
        for edge in &self.successors {
            self.blocks[edge.block.index()].first_predecessor += 1;
        }
        let mut end = 0;
        for block in &mut self.blocks {
            end += block.first_predecessor;
            block.first_predecessor = end;
        }
        let Some(&any) = self.successors.first() else {
            self.predecessors.clear();
            return Ok(());
        };
        refill(&mut self.predecessors, self.successors.len(), any)?;
        for index in (0..self.len()).rev() {
            let first = self.blocks[index].first_successor as usize;
            let end = self.blocks[index + 1].first_successor as usize;
            for edge in self.successors[first..end].iter().rev() {
                let at = &mut self.blocks[edge.block.index()].first_predecessor;
                *at -= 1;
                self.predecessors[*at as usize] = Edge {
                    block: BlockIndex(index as u32),
                    kind: edge.kind,
                };
            }
        }
        Ok(())
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.blocks.len() - 1
    }

    pub fn blocks(&self) -> impl DoubleEndedIterator<Item = BlockIndex> + use<> {
        (0..self.len() as u32).map(BlockIndex)
    }

    /// The one that has the byte at `offset`.
    #[inline]
    pub fn block_at(&self, offset: u32) -> BlockIndex {
        let (bits, before) = self.leaders[offset as usize / 64];
        BlockIndex(before + (bits << (63 - offset % 64)).count_ones() - 1)
    }

    #[inline]
    pub fn range(&self, block: BlockIndex) -> Range<u32> {
        self.blocks[block.index()].start..self.blocks[block.index() + 1].start
    }

    #[inline]
    pub fn instructions<'a>(&self, body: &'a Body, block: BlockIndex) -> Instructions<'a> {
        Instructions::new(body.instructions(), self.range(block))
    }

    /// Whether or not anything in the block throws.
    #[inline]
    pub fn handler(&self, block: BlockIndex) -> Option<BlockIndex> {
        let handler = self.blocks[block.index()].handler;
        (handler != NONE).then_some(BlockIndex(handler))
    }

    /// No edge comes twice.
    #[inline]
    pub fn successors(&self, block: BlockIndex) -> &[Edge] {
        let first = self.blocks[block.index()].first_successor;
        let end = self.blocks[block.index() + 1].first_successor;
        &self.successors[first as usize..end as usize]
    }

    #[inline]
    pub fn predecessors(&self, block: BlockIndex) -> &[Edge] {
        let first = self.blocks[block.index()].first_predecessor;
        let end = self.blocks[block.index() + 1].first_predecessor;
        &self.predecessors[first as usize..end as usize]
    }
}
