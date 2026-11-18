use core::cell::Cell;

use bun_threading::Guarded;

use super::allocator::Allocator;
use super::block::{Block, FreeList, PAYLOAD_SIZE};
use super::block_memory::BlockSource;
use super::{CellAttributes, Destruction, Heap, Scope};

macro_rules! bits {
    ($($(#[$doc:meta])* $name:ident,)*) => {
        /// What is known of 64 blocks, a bit for each.
        #[derive(Clone, Copy, Default)]
        struct Bits {
            $($(#[$doc])* $name: u64,)*
        }

        impl Bits {
            fn clear_all(&mut self, bit: u64) {
                $(self.$name &= !bit;)*
            }
        }
    };
}

bits! {
    present,
    /// No live cell.
    empty,
    /// Enough free cells to be worth a sweep.
    can_allocate,
    /// Allocated from since the last collection.
    eden,
    /// Its cells have to be destroyed when they die.
    destructible,
    /// Some that have died have not been destroyed.
    unswept,
    /// The last full collection has left room for a page without a live cell, and nothing has been done about it.
    sparse,
    /// The marks are current.
    marked,
    /// So many are marked that the rest is not worth a sweep.
    retired,
}

fn take_bit(word: &mut u64, bit: u64) -> bool {
    let was_set = *word & bit != 0;
    *word &= !bit;
    was_set
}

fn set_bits(word: u64) -> impl Iterator<Item = usize> {
    let rests = core::iter::successors(Some(word), |rest| Some(rest & rest.wrapping_sub(1)));
    (rests.take_while(|&rest| rest != 0)).map(|rest| rest.trailing_zeros() as usize)
}

#[derive(Default)]
struct Blocks {
    blocks: Vec<Option<&'static Block>>,
    bits: Vec<Bits>,
    free_indices: Vec<u32>,
}

impl Blocks {
    #[inline]
    fn bits_of(&mut self, block: &Block) -> (&mut Bits, u64) {
        let index = block.index.get() as usize;
        (&mut self.bits[index / 64], 1 << (index % 64))
    }

    /// The first block in the words from `cursor` on that `select` has a bit for.
    fn find(&self, cursor: &Cell<usize>, select: impl Fn(&Bits) -> u64) -> Option<&'static Block> {
        while let Some(bits) = self.bits.get(cursor.get()) {
            if let Some(index) = set_bits(select(bits)).next() {
                return self.blocks[cursor.get() * 64 + index];
            }
            cursor.set(cursor.get() + 1);
        }
        None
    }

    fn selected(&self, select: impl Fn(&Bits) -> u64) -> Vec<&'static Block> {
        let mut selected = Vec::new();
        for (bits, blocks) in self.bits.iter().zip(self.blocks.chunks(64)) {
            selected.extend(set_bits(select(bits)).filter_map(|index| blocks[index]));
        }
        selected
    }

    /// Whether it is unswept.
    fn remove(&mut self, block: &Block) -> bool {
        let (bits, bit) = self.bits_of(block);
        let is_unswept = bits.unswept & bit != 0;
        bits.clear_all(bit);
        self.blocks[block.index.get() as usize] = None;
        self.free_indices.push(block.index.get());
        is_unswept
    }
}

/// The blocks of an allocator.
pub(super) struct Directory {
    heap: *const Heap,
    allocator: *const Allocator,
    blocks: Guarded<Blocks>,
    allocation_cursor: Cell<usize>,
    empty_cursor: Cell<usize>,
    swept_empty_cursor: Cell<usize>,
    unswept_cursor: Cell<usize>,
    sparse_cursor: Cell<usize>,
}

impl Directory {
    pub(super) fn new(heap: &Heap, allocator: &Allocator) -> Directory {
        Directory {
            heap,
            allocator,
            blocks: Guarded::default(),
            allocation_cursor: Cell::new(0),
            empty_cursor: Cell::new(0),
            swept_empty_cursor: Cell::new(0),
            unswept_cursor: Cell::new(0),
            sparse_cursor: Cell::new(0),
        }
    }

    pub(super) fn heap(&self) -> &Heap {
        // SAFETY: the heap owns the allocator, which owns the directory, and neither moves.
        unsafe { &*self.heap }
    }

    pub(super) fn allocator(&self) -> &Allocator {
        // SAFETY: as above.
        unsafe { &*self.allocator }
    }

    pub(super) fn cell_size(&self) -> usize {
        self.allocator().cell_size()
    }

    pub(super) fn attributes(&self) -> CellAttributes {
        self.allocator().attributes
    }

    pub(super) fn did_prepare_for_marking(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.marked |= bit;
    }

    pub(super) fn did_request_destruction(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.destructible |= bit;
    }

    pub(super) fn did_stop_allocating(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.can_allocate |= bit;
        let word = block.index.get() as usize / 64;
        self.allocation_cursor
            .set(self.allocation_cursor.get().min(word));
    }

    fn destroy_dead_cells(&self, block: &Block) {
        let _state = self.heap().enter_destruction();
        // SAFETY: only the thread of the heap sweeps.
        unsafe { block.destroy_dead_cells() }
    }

    /// For a block that stays in the directory.
    fn sweep(&self, block: &Block) {
        self.destroy_dead_cells(block);
        if self.attributes().destruction == Destruction::OnRequest
            && !block.is_destruction_requested()
        {
            let mut blocks = self.blocks.lock();
            let (bits, bit) = blocks.bits_of(block);
            bits.destructible &= !bit;
        }
    }

    pub(super) fn find_free_cells(&self, secret: u64) -> Option<(&'static Block, FreeList)> {
        loop {
            let mut blocks = self.blocks.lock();
            let block = blocks.find(&self.allocation_cursor, |bits| {
                bits.can_allocate | bits.empty
            })?;
            let (bits, bit) = blocks.bits_of(block);
            let is_unswept = take_bit(&mut bits.unswept, bit);
            bits.can_allocate &= !bit;
            bits.empty &= !bit;
            bits.sparse &= !bit;
            bits.eden |= bit;
            drop(blocks);
            if is_unswept {
                self.sweep(block);
            }
            // SAFETY: swept.
            let free_list = unsafe { block.build_free_list(secret) };
            if free_list.first_interval != 0 {
                return Some((block, free_list));
            }
            block.did_consume_free_list();
        }
    }

    /// # Safety
    /// `memory` is an unused block from the source of the allocator.
    pub(super) unsafe fn add_block(
        &self,
        memory: usize,
        secret: u64,
    ) -> (&'static Block, FreeList) {
        // SAFETY: the caller's, and the heap frees the block before the directory goes.
        let block = unsafe { Block::initialize(memory, self) };
        let mut blocks = self.blocks.lock();
        let index = blocks.free_indices.pop().unwrap_or_else(|| {
            blocks.blocks.push(None);
            let words = blocks.blocks.len().div_ceil(64);
            blocks.bits.resize(words, Bits::default());
            blocks.blocks.len() as u32 - 1
        });
        blocks.blocks[index as usize] = Some(block);
        block.index.set(index);
        let (bits, bit) = blocks.bits_of(block);
        bits.present |= bit;
        bits.eden |= bit;
        if self.attributes().destroys_every_cell() {
            bits.destructible |= bit;
        }
        // SAFETY: nothing is in it.
        (block, unsafe { block.build_free_list(secret) })
    }

    /// Removes an empty block and returns its memory, in which everything has been destroyed.
    pub(super) fn take_empty_block(&self) -> Option<usize> {
        let mut blocks = self.blocks.lock();
        let block = blocks.find(&self.empty_cursor, |bits| bits.empty)?;
        let is_unswept = blocks.remove(block);
        drop(blocks);
        if is_unswept {
            self.destroy_dead_cells(block);
        }
        block.reuse_discarded_pages();
        Some(block.address())
    }

    /// The same, but only one in which nothing is left to destroy.
    pub(super) fn take_swept_empty_block(&self) -> Option<usize> {
        let mut blocks = self.blocks.lock();
        let block = blocks.find(&self.swept_empty_cursor, |bits| bits.empty & !bits.unswept)?;
        blocks.remove(block);
        block.reuse_discarded_pages();
        Some(block.address())
    }

    pub(super) fn begin_full_marking(&self) {
        for bits in &mut self.blocks.lock().bits {
            bits.marked = 0;
            bits.retired = 0;
        }
    }

    /// Returns the bytes of the cells that this collection has marked.
    pub(super) fn end_marking(&self, scope: Scope) -> usize {
        let cells = (PAYLOAD_SIZE / self.cell_size()) as u32;
        let retire_count = (cells * 9).div_ceil(10);
        let sparse_count = self.max_marks_of_sparse_block(cells);
        let mut newly_marked = 0;
        let Blocks { blocks, bits, .. } = &mut *self.blocks.lock();
        for (bits, blocks) in bits.iter_mut().zip(blocks.chunks(64)) {
            let examined = match scope {
                Scope::Full => bits.present,
                Scope::Eden => bits.eden,
            };
            if scope == Scope::Full {
                bits.sparse = 0;
            }
            bits.unswept |= examined & bits.destructible & !bits.empty;
            for index in set_bits(examined & bits.marked) {
                let (count, new) = blocks[index].expect("present").count_marks();
                newly_marked += new as usize;
                bits.retired |= u64::from(count >= retire_count) << index;
                bits.sparse |= u64::from(scope == Scope::Full && count < sparse_count) << index;
                bits.unswept &= !(u64::from(count == cells) << index);
            }
            bits.empty = bits.present & !bits.marked;
            bits.can_allocate = bits.marked & !bits.retired;
            bits.eden = 0;
        }
        for cursor in [
            &self.allocation_cursor,
            &self.empty_cursor,
            &self.swept_empty_cursor,
            &self.unswept_cursor,
            &self.sparse_cursor,
        ] {
            cursor.set(0);
        }
        newly_marked * self.cell_size()
    }

    /// One more than that, and zero if pages of these blocks are not given back.
    fn max_marks_of_sparse_block(&self, cells: u32) -> u32 {
        // Elsewhere, reading a page that has been given back is no fault. A dead shape may still be read.
        if cfg!(windows) || self.allocator().source == BlockSource::Shapes {
            return 0;
        }
        let cells_in_page = bun_alloc::page_size().div_ceil(self.cell_size()) as u32;
        (cells + 1).saturating_sub(cells_in_page)
    }

    /// False if there is nothing to sweep.
    pub(super) fn sweep_next_block(&self) -> bool {
        let mut blocks = self.blocks.lock();
        let Some(block) = blocks.find(&self.unswept_cursor, |bits| bits.unswept) else {
            return false;
        };
        let (bits, bit) = blocks.bits_of(block);
        bits.unswept &= !bit;
        drop(blocks);
        self.sweep(block);
        let word = block.index.get() as usize / 64;
        self.swept_empty_cursor
            .set(self.swept_empty_cursor.get().min(word));
        true
    }

    /// False if there is none. Nothing is unswept.
    pub(super) fn discard_pages_of_next_sparse_block(&self) -> bool {
        let mut blocks = self.blocks.lock();
        let Some(block) = blocks.find(&self.sparse_cursor, |bits| bits.sparse) else {
            return false;
        };
        let (bits, bit) = blocks.bits_of(block);
        assert!(bits.unswept & bit == 0);
        bits.sparse &= !bit;
        // SAFETY: swept, and not allocated from since the last collection.
        unsafe { block.discard_unused_pages() };
        true
    }

    /// Nothing is unswept.
    #[cold]
    pub(super) fn scribble_on_dead_cells(&self) {
        let blocks = self.blocks.lock();
        for block in blocks.selected(|bits| bits.present & !bits.eden & !bits.unswept) {
            // SAFETY: swept, and not allocated from since the last collection.
            unsafe { block.scribble_on_dead_cells() };
        }
    }

    pub(super) fn blocks(&self) -> Vec<&'static Block> {
        self.blocks.lock().selected(|bits| bits.present)
    }

    pub(super) fn blocks_with_marks(&self) -> Vec<&'static Block> {
        self.blocks.lock().selected(|bits| bits.marked)
    }

    pub(super) fn empty_block_count(&self) -> usize {
        let blocks = self.blocks.lock();
        let counts = blocks.bits.iter().map(|bits| bits.empty.count_ones());
        counts.sum::<u32>() as usize
    }
}
