//! Finds the cells that words of unknown meaning may refer to.

use super::block::{BLOCK_SIZE, Block};
use super::block_set::BlockSet;
use super::precise::PreciseAllocation;
use super::stack::load_word;
use super::{AllocationKind, block_memory};

#[derive(Default)]
pub(super) struct Found {
    pub(super) cells: Vec<usize>,
    pub(super) auxiliary: Vec<usize>,
    pub(super) code_addresses: Vec<usize>,
}

pub(super) struct ConservativeRoots<'a> {
    blocks: &'a BlockSet,
    /// Sorted by address.
    precise: Vec<&'static PreciseAllocation>,
    code: core::ops::Range<usize>,
    pub(super) found: Found,
}

const ADDRESS_BITS: u32 = 48;

/// How far past the end of a storage the pointer to its elements can be.
const PAST_THE_END: usize = size_of::<u64>() + 1;

impl<'a> ConservativeRoots<'a> {
    pub(super) fn new(
        blocks: &'a BlockSet,
        precise: &[&'static PreciseAllocation],
        code: core::ops::Range<usize>,
    ) -> ConservativeRoots<'a> {
        let mut precise = precise.to_vec();
        precise.sort_unstable_by_key(|allocation| allocation.cell());
        ConservativeRoots {
            blocks,
            precise,
            code,
            found: Found::default(),
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
        // A native function may have signed the return address that it saved.
        let code_address = if cfg!(target_arch = "aarch64") {
            word & ((1 << ADDRESS_BITS) - 1)
        } else {
            word
        };
        if self.code.contains(&code_address) {
            return self.found.code_addresses.push(code_address);
        }
        if word >> ADDRESS_BITS != 0 || word < BLOCK_SIZE {
            return;
        }
        if let (Some(first), Some(last)) = (self.precise.first(), self.precise.last())
            && word >= first.cell()
            && (word < last.cell() || last.contains(word))
        {
            let after = self.precise.partition_point(|it| it.cell() <= word);
            let allocation = self.precise[after - 1];
            if allocation.contains(word) {
                if allocation.is_live() {
                    self.did_find(allocation.cell(), allocation.attributes.kind);
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
            self.did_find(cell, block.attributes.kind);
        }
    }

    fn did_find(&mut self, cell: usize, kind: AllocationKind) {
        match kind {
            AllocationKind::Cell => self.found.cells.push(cell),
            AllocationKind::Auxiliary => self.found.auxiliary.push(cell),
        }
    }
}
