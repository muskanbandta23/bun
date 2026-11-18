use core::cell::Cell;

use bun_threading::Guarded;

use super::allocator::Allocator;
use super::block::{Block, SweepResult};
use super::{CellAttributes, Heap, MutatorState, Scope};

/// What is known of 64 blocks, a bit for each.
#[derive(Clone, Copy, Default)]
struct Bits {
    present: u64,
    /// No live cell.
    empty: u64,
    /// Enough free cells to be worth a sweep.
    can_allocate: u64,
    /// Allocated from since the last collection.
    eden: u64,
    unswept: u64,
    marking_not_empty: u64,
    marking_retired: u64,
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
            let selected = select(bits);
            if selected != 0 {
                return self.blocks[cursor.get() * 64 + selected.trailing_zeros() as usize];
            }
            cursor.set(cursor.get() + 1);
        }
        None
    }

    fn selected(&self, select: impl Fn(&Bits) -> u64) -> Vec<&'static Block> {
        let mut selected = Vec::new();
        for (word, bits) in self.bits.iter().enumerate() {
            let mut rest = select(bits);
            while rest != 0 {
                selected.extend(self.blocks[word * 64 + rest.trailing_zeros() as usize]);
                rest &= rest - 1;
            }
        }
        selected
    }
}

/// The blocks of an allocator.
pub(super) struct Directory {
    heap: *const Heap,
    allocator: *const Allocator,
    blocks: Guarded<Blocks>,
    allocation_cursor: Cell<usize>,
    empty_cursor: Cell<usize>,
    unswept_cursor: Cell<usize>,
}

impl Directory {
    pub(super) fn new(heap: &Heap, allocator: &Allocator) -> Directory {
        Directory {
            heap,
            allocator,
            blocks: Guarded::default(),
            allocation_cursor: Cell::new(0),
            empty_cursor: Cell::new(0),
            unswept_cursor: Cell::new(0),
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

    pub(super) fn did_start_marking(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.marking_not_empty |= bit;
    }

    #[cold]
    pub(super) fn did_retire(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.marking_retired |= bit;
    }

    pub(super) fn did_stop_allocating(&self, block: &Block) {
        let mut blocks = self.blocks.lock();
        let (bits, bit) = blocks.bits_of(block);
        bits.can_allocate |= bit;
        let word = block.index.get() as usize / 64;
        self.allocation_cursor
            .set(self.allocation_cursor.get().min(word));
    }

    fn sweep(&self, block: &Block, secret: Option<u64>) -> SweepResult {
        let _state = self.heap().enter_state(MutatorState::Sweeping);
        // SAFETY: only the thread of the heap sweeps.
        unsafe { block.sweep(secret) }
    }

    pub(super) fn find_free_cells(&self, secret: u64) -> Option<(&'static Block, SweepResult)> {
        loop {
            let block = {
                let mut blocks = self.blocks.lock();
                let found = blocks.find(&self.allocation_cursor, |bits| {
                    bits.can_allocate | bits.empty
                });
                let Some(block) = found else { break };
                let (bits, bit) = blocks.bits_of(block);
                bits.can_allocate &= !bit;
                bits.empty &= !bit;
                bits.unswept &= !bit;
                bits.eden |= bit;
                block
            };
            let swept = self.sweep(block, Some(secret));
            if swept.first_interval != 0 {
                return Some((block, swept));
            }
            block.did_consume_free_list();
        }
        let block = self.add_block()?;
        Some((block, self.sweep(block, Some(secret))))
    }

    fn add_block(&self) -> Option<&'static Block> {
        let memory = self.heap().acquire_block(self.allocator().source)?;
        // SAFETY: unused memory, and the directory frees the block before it goes.
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
        Some(block)
    }

    /// Removes an empty block and returns its memory, in which everything has been destroyed.
    pub(super) fn take_empty_block(&self) -> Option<usize> {
        let block = {
            let mut blocks = self.blocks.lock();
            let block = blocks.find(&self.empty_cursor, |bits| bits.empty)?;
            let (bits, bit) = blocks.bits_of(block);
            for word in [
                &mut bits.present,
                &mut bits.empty,
                &mut bits.can_allocate,
                &mut bits.eden,
                &mut bits.unswept,
                &mut bits.marking_not_empty,
                &mut bits.marking_retired,
            ] {
                *word &= !bit;
            }
            blocks.blocks[block.index.get() as usize] = None;
            blocks.free_indices.push(block.index.get());
            block
        };
        if block.attributes.destroy.is_some() && block.has_touched_cells() {
            self.sweep(block, None);
        }
        Some(block.address())
    }

    pub(super) fn begin_full_marking(&self) {
        for bits in &mut self.blocks.lock().bits {
            bits.marking_not_empty = 0;
            bits.marking_retired = 0;
        }
    }

    pub(super) fn end_marking(&self, scope: Scope) {
        let needs_destruction = self.attributes().destroy.is_some();
        for bits in &mut self.blocks.lock().bits {
            bits.empty = bits.present & !bits.marking_not_empty;
            bits.can_allocate = bits.present & bits.marking_not_empty & !bits.marking_retired;
            if needs_destruction {
                bits.unswept |= match scope {
                    Scope::Full => bits.present,
                    Scope::Eden => bits.eden,
                };
            }
            bits.eden = 0;
        }
        self.allocation_cursor.set(0);
        self.empty_cursor.set(0);
        self.unswept_cursor.set(0);
    }

    /// False if there is nothing to sweep.
    pub(super) fn sweep_next_block(&self) -> bool {
        let block = {
            let mut blocks = self.blocks.lock();
            let Some(block) = blocks.find(&self.unswept_cursor, |bits| bits.unswept) else {
                return false;
            };
            let (bits, bit) = blocks.bits_of(block);
            bits.unswept &= !bit;
            block
        };
        self.sweep(block, None);
        true
    }

    pub(super) fn blocks(&self) -> Vec<&'static Block> {
        self.blocks.lock().selected(|bits| bits.present)
    }

    pub(super) fn blocks_with_marks(&self) -> Vec<&'static Block> {
        self.blocks.lock().selected(|bits| bits.marking_not_empty)
    }

    pub(super) fn empty_block_count(&self) -> usize {
        let blocks = self.blocks.lock();
        blocks
            .bits
            .iter()
            .map(|bits| bits.empty.count_ones() as usize)
            .sum()
    }
}
