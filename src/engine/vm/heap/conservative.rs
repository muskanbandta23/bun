//! Finds the cells that words of unknown meaning may refer to.

use super::block::{BLOCK_SIZE, Block};
use super::block_set::BlockSet;
use super::precise::PreciseAllocation;
use super::stack::load_word;
use super::{AllocationKind, block_memory};

pub(super) struct ConservativeRoots<'a> {
    blocks: &'a BlockSet,
    /// Sorted by address.
    precise: &'a [&'static PreciseAllocation],
    code: core::ops::Range<usize>,
    pub(super) cells: Vec<usize>,
    pub(super) auxiliary: Vec<usize>,
    pub(super) code_addresses: Vec<usize>,
}

/// How far past the end of a storage the pointer to its elements can be.
const PAST_THE_END: usize = size_of::<u64>() + 1;

impl<'a> ConservativeRoots<'a> {
    pub(super) fn new(
        blocks: &'a BlockSet,
        precise: &'a [&'static PreciseAllocation],
        code: core::ops::Range<usize>,
    ) -> ConservativeRoots<'a> {
        ConservativeRoots {
            blocks,
            precise,
            code,
            cells: Vec::new(),
            auxiliary: Vec::new(),
            code_addresses: Vec::new(),
        }
    }

    /// # Safety
    /// The range is mapped.
    pub(super) unsafe fn scan(&mut self, start: usize, end: usize) {
        const WORD: usize = size_of::<usize>();
        let shapes = block_memory::shape_base();
        let shape_ids = BLOCK_SIZE..block_memory::shape_id_limit();
        for address in (start.next_multiple_of(WORD)..end & !(WORD - 1)).step_by(WORD) {
            // SAFETY: the caller's.
            let word = unsafe { load_word(address) };
            self.add(word);
            // The id of a shape, which is all that a frame may have of it.
            for half in [word as u32, (word >> 32) as u32] {
                if shape_ids.contains(&(half as usize)) {
                    self.add_in_blocks(shapes + half as usize);
                }
            }
        }
    }

    #[inline]
    fn add(&mut self, word: usize) {
        if word >> 48 != 0 || word < BLOCK_SIZE {
            return;
        }
        if self.code.contains(&word) {
            return self.code_addresses.push(word);
        }
        if let (Some(first), Some(last)) = (self.precise.first(), self.precise.last())
            && word >= first.cell()
            && (word < last.cell() || last.contains(word))
        {
            let after = self.precise.partition_point(|it| it.cell() <= word);
            let allocation = self.precise[after - 1];
            if allocation.contains(word) {
                if allocation.is_live() {
                    self.found(allocation.cell(), allocation.attributes.kind);
                }
                return;
            }
        }
        self.add_in_blocks(word);
        if word % BLOCK_SIZE < PAST_THE_END
            && let Some(block) = self.blocks.block_containing(word - PAST_THE_END)
            && block.attributes.kind == AllocationKind::Auxiliary
        {
            self.add_in_block(block, word - PAST_THE_END);
        }
    }

    #[inline]
    fn add_in_blocks(&mut self, word: usize) {
        if let Some(block) = self.blocks.block_containing(word) {
            self.add_in_block(block, word);
            if block.attributes.kind == AllocationKind::Auxiliary
                && word % BLOCK_SIZE >= PAST_THE_END
            {
                self.add_in_block(block, word - PAST_THE_END);
            }
        }
    }

    #[inline]
    fn add_in_block(&mut self, block: &Block, word: usize) {
        if let Some(cell) = block.cell_containing(word)
            && block.is_live(cell)
        {
            self.found(cell, block.attributes.kind);
        }
    }

    fn found(&mut self, cell: usize, kind: AllocationKind) {
        match kind {
            AllocationKind::Cell => self.cells.push(cell),
            AllocationKind::Auxiliary => self.auxiliary.push(cell),
        }
    }
}
