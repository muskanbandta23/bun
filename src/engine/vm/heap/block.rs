use core::cell::Cell;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bun_alloc::page_size;
use bun_threading::Mutex;

use super::directory::Directory;
use super::stack::prefetch;
use super::{CellAttributes, Destruction, Heap, JsCell, virtual_memory};
use crate::{JsString, options};

pub const BLOCK_SIZE: usize = 16 * 1024;
pub const ATOM_SIZE: usize = 16;
const ATOMS_PER_BLOCK: usize = BLOCK_SIZE / ATOM_SIZE;
const BITMAP_WORDS: usize = ATOMS_PER_BLOCK / 64;
/// The same on every target, and so are the size classes.
const HEADER_ATOMS: usize = 24;
const _: () = assert!(size_of::<Block>() <= HEADER_ATOMS * ATOM_SIZE);
pub const PAYLOAD_SIZE: usize = BLOCK_SIZE - HEADER_ATOMS * ATOM_SIZE;
const RECIPROCAL_SHIFT: u32 = 20;

/// Bits stamped with another version than the heap's are stale, so nothing has to visit every block to clear them.
pub(super) type Version = u32;
pub(super) const NULL_VERSION: Version = 0;
/// That of no block.
pub(super) const UNUSED_VERSION: Version = 1;
pub(super) const FIRST_VERSION: Version = 2;

pub(super) fn next_version(version: Version) -> Version {
    version.checked_add(1).unwrap_or(FIRST_VERSION)
}

type Bits = [u64; BITMAP_WORDS];

/// One bit for each atom. Only that of the first atom of a cell is used.
#[repr(C, align(64))]
struct Bitmap([AtomicU64; BITMAP_WORDS]);

impl Bitmap {
    #[inline(always)]
    fn word(&self, atom: usize) -> &AtomicU64 {
        &self.0[atom / 64 % BITMAP_WORDS]
    }

    #[inline(always)]
    fn get(&self, atom: usize) -> bool {
        self.word(atom).load(Ordering::Relaxed) >> (atom % 64) & 1 != 0
    }

    /// Whether it was set.
    #[inline(always)]
    fn test_and_set(&self, atom: usize) -> bool {
        let bit = 1 << (atom % 64);
        self.word(atom).fetch_or(bit, Ordering::Relaxed) & bit != 0
    }

    #[inline(always)]
    fn set_exclusively(&self, atom: usize) {
        let word = self.word(atom);
        word.store(
            word.load(Ordering::Relaxed) | 1 << (atom % 64),
            Ordering::Relaxed,
        );
    }

    fn snapshot(&self) -> Bits {
        self.0.each_ref().map(|word| word.load(Ordering::Relaxed))
    }

    fn assign(&self, words: &Bits) {
        for (word, &value) in self.0.iter().zip(words) {
            word.store(value, Ordering::Relaxed);
        }
    }

    fn count(&self) -> u32 {
        self.snapshot().iter().map(|word| word.count_ones()).sum()
    }
}

#[inline(always)]
fn for_each_set_bit(bits: &Bits, mut visit: impl FnMut(usize)) {
    for (index, &word) in bits.iter().enumerate() {
        let mut rest = word;
        while rest != 0 {
            visit(index * 64 + rest.trailing_zeros() as usize);
            rest &= rest - 1;
        }
    }
}

/// The header of an aligned run of memory that is cut into cells of one size. Markers write nothing to the cache line
/// that they all read the version from.
#[repr(C)]
pub struct Block {
    heap: *const Heap,
    marking_version: AtomicU32,
    atoms_per_cell: u32,
    directory: *const Directory,
    pub(super) attributes: CellAttributes,
    first_atom: u32,
    reciprocal: u32,
    pub(super) index: Cell<u32>,
    marks: Bitmap,
    /// How many of the marks earlier collections have counted.
    counted_marks: AtomicU32,
    /// The cells from this offset on have never been allocated, and their memory has not been touched.
    virgin_offset: Cell<u32>,
    /// Current: every cell has been allocated or has survived since the last collection.
    allocated_version: AtomicU32,
    /// Current: `newly_allocated` says which cells are live.
    newly_allocated_version: AtomicU32,
    is_free_listed: Cell<bool>,
    /// For `Destruction::OnRequest`.
    is_destruction_requested: Cell<bool>,
    /// A bit for each page that has been given back to the system. No live cell is in it.
    discarded_pages: Cell<u8>,
    lock: Mutex,
    newly_allocated: Bitmap,
}

/// A run of free cells. Its first cell says where the next one is.
#[repr(C)]
pub(super) struct FreeInterval {
    /// Zero: the mark of a destroyed cell.
    zapped_header: u64,
    scrambled: u64,
}

impl FreeInterval {
    /// The length in bytes, and the address of the next interval or zero.
    ///
    /// # Safety
    /// `address` is the start of an interval that was written with `secret`.
    #[inline(always)]
    pub(super) unsafe fn read(address: usize, secret: u64) -> (usize, usize) {
        // SAFETY: the caller's.
        let interval = unsafe { &*core::ptr::with_exposed_provenance::<FreeInterval>(address) };
        let bits = interval.scrambled ^ secret;
        let (length, offset) = ((bits >> 32) as usize, bits as u32 as usize);
        (length, if offset == 0 { 0 } else { address + offset })
    }

    /// # Safety
    /// `address` is a free cell.
    #[inline(always)]
    unsafe fn write(address: usize, length: usize, next: usize, secret: u64) {
        let offset = next.saturating_sub(address);
        // SAFETY: a cell has room for it.
        unsafe {
            core::ptr::with_exposed_provenance_mut::<FreeInterval>(address).write(FreeInterval {
                zapped_header: 0,
                scrambled: ((length as u64) << 32 | offset as u64) ^ secret,
            });
        }
    }
}

#[derive(Default)]
pub(super) struct FreeList {
    /// Zero if no cell is free.
    pub(super) first_interval: usize,
    pub(super) bytes: usize,
}

impl Block {
    pub const OFFSET_OF_HEAP: usize = core::mem::offset_of!(Block, heap);

    /// # Safety
    /// `memory` is an unused block. `directory` and its heap outlive it.
    pub(super) unsafe fn initialize(memory: usize, directory: &Directory) -> &'static Block {
        let atoms_per_cell = directory.cell_size() / ATOM_SIZE;
        let cells = PAYLOAD_SIZE / directory.cell_size();
        let first_atom = ATOMS_PER_BLOCK - cells * atoms_per_cell;
        let block = core::ptr::with_exposed_provenance_mut::<Block>(memory);
        let empty = || Bitmap([const { AtomicU64::new(0) }; BITMAP_WORDS]);
        // SAFETY: the caller's.
        unsafe {
            block.write(Block {
                heap: directory.heap(),
                marking_version: AtomicU32::new(NULL_VERSION),
                atoms_per_cell: atoms_per_cell as u32,
                directory,
                attributes: directory.attributes(),
                first_atom: first_atom as u32,
                reciprocal: (1u32 << RECIPROCAL_SHIFT).div_ceil(atoms_per_cell as u32),
                index: Cell::new(0),
                marks: empty(),
                counted_marks: AtomicU32::new(0),
                virgin_offset: Cell::new((first_atom * ATOM_SIZE) as u32),
                allocated_version: AtomicU32::new(NULL_VERSION),
                newly_allocated_version: AtomicU32::new(NULL_VERSION),
                is_free_listed: Cell::new(false),
                is_destruction_requested: Cell::new(false),
                discarded_pages: Cell::new(0),
                lock: Mutex::new(),
                newly_allocated: empty(),
            });
            &*block
        }
    }

    /// # Safety
    /// `address` is inside a block.
    #[inline(always)]
    pub(super) unsafe fn containing(address: usize) -> &'static Block {
        // SAFETY: the caller's.
        unsafe { &*core::ptr::with_exposed_provenance(address & !(BLOCK_SIZE - 1)) }
    }

    #[inline(always)]
    pub(super) fn address(&self) -> usize {
        core::ptr::from_ref(self).addr()
    }

    #[inline(always)]
    pub(super) fn heap(&self) -> &Heap {
        // SAFETY: outlives the block.
        unsafe { &*self.heap }
    }

    #[inline(always)]
    pub(super) fn directory(&self) -> &Directory {
        // SAFETY: outlives the block.
        unsafe { &*self.directory }
    }

    #[inline(always)]
    pub(super) fn cell_size(&self) -> usize {
        self.atoms_per_cell as usize * ATOM_SIZE
    }

    #[inline(always)]
    fn atom_of(address: usize) -> usize {
        address % BLOCK_SIZE / ATOM_SIZE
    }

    fn first_cell(&self) -> usize {
        self.address() + self.first_atom as usize * ATOM_SIZE
    }

    fn end(&self) -> usize {
        self.address() + BLOCK_SIZE
    }

    /// The start of the cell that an address of this block is in.
    #[inline(always)]
    pub(super) fn cell_containing(&self, address: usize) -> Option<usize> {
        let atom = (Self::atom_of(address) as u32).checked_sub(self.first_atom)?;
        let index = (atom * self.reciprocal) >> RECIPROCAL_SHIFT;
        let first_atom = self.first_atom + index * self.atoms_per_cell;
        Some(self.address() + first_atom as usize * ATOM_SIZE)
    }

    #[inline(always)]
    pub(super) fn has_marks_of(&self, version: Version) -> bool {
        self.marking_version.load(Ordering::Acquire) == version
    }

    #[inline(always)]
    pub(super) fn is_marked(&self, version: Version, cell: usize) -> bool {
        self.has_marks_of(version) && self.has_mark(cell)
    }

    /// Whichever version the marks are of.
    #[inline(always)]
    pub(super) fn has_mark(&self, cell: usize) -> bool {
        self.marks.get(Self::atom_of(cell))
    }

    /// Whether the cell was marked already. The marks are current.
    #[inline(always)]
    pub(super) fn test_and_set_mark(&self, cell: usize) -> bool {
        self.marks.test_and_set(Self::atom_of(cell))
    }

    /// The marks are current, and no other marker is running.
    #[inline(always)]
    pub(super) fn set_mark_exclusively(&self, cell: usize) {
        self.marks.set_exclusively(Self::atom_of(cell));
    }

    /// Makes the marks current. False if they were.
    pub(super) fn prepare_for_marking(&self, version: Version) -> bool {
        let _lock = self.lock.lock_guard();
        let old_version = self.marking_version.load(Ordering::Relaxed);
        if old_version == version {
            return false;
        }
        let allocation_version = self.heap().allocation_version();
        if next_version(old_version) == version
            && self.allocated_version.load(Ordering::Relaxed) != allocation_version
            && self.newly_allocated_version.load(Ordering::Relaxed) != allocation_version
        {
            // What the old marks say about liveness is needed until this collection ends.
            self.newly_allocated.assign(&self.marks.snapshot());
            self.newly_allocated_version
                .store(allocation_version, Ordering::Release);
        }
        self.marks.assign(&Bits::default());
        self.counted_marks.store(0, Ordering::Relaxed);
        self.marking_version.store(version, Ordering::Release);
        true
    }

    /// How many cells are marked, and how many of them since the last call. The marks are current.
    pub(super) fn count_marks(&self) -> (u32, u32) {
        let count = self.marks.count();
        let counted = self.counted_marks.load(Ordering::Relaxed);
        self.counted_marks.store(count, Ordering::Relaxed);
        (count, count - counted)
    }

    /// As of the end of the last collection. The marks are current.
    pub(super) fn counted_marks(&self) -> usize {
        self.counted_marks.load(Ordering::Relaxed) as usize
    }

    fn marks_convey_liveness(&self) -> bool {
        let (heap, version) = (self.heap(), self.marking_version.load(Ordering::Relaxed));
        version == heap.marking_version()
            || (heap.is_marking_full() && next_version(version) == heap.marking_version())
    }

    /// Whether the cell survived the last collection or was allocated since. No marker is running, and the block is
    /// not being allocated from.
    pub(super) fn is_live(&self, cell: usize) -> bool {
        debug_assert!(!self.is_free_listed.get());
        let version = self.heap().allocation_version();
        if self.allocated_version.load(Ordering::Relaxed) == version {
            return true;
        }
        if self.newly_allocated_version.load(Ordering::Relaxed) == version {
            return self.newly_allocated.get(Self::atom_of(cell));
        }
        self.marks_convey_liveness() && self.has_mark(cell)
    }

    pub(super) fn is_free_listed(&self) -> bool {
        self.is_free_listed.get()
    }

    /// The block does not consist of live cells only.
    fn live_bits(&self) -> Bits {
        let _lock = self.lock.lock_guard();
        let version = self.heap().allocation_version();
        debug_assert!(self.allocated_version.load(Ordering::Relaxed) != version);
        if self.newly_allocated_version.load(Ordering::Relaxed) == version {
            self.newly_allocated.snapshot()
        } else if self.marks_convey_liveness() {
            self.marks.snapshot()
        } else {
            Bits::default()
        }
    }

    /// As for `is_live`.
    pub(super) fn for_each_live_cell(&self, mut visit: impl FnMut(usize)) {
        debug_assert!(!self.is_free_listed.get());
        if self.allocated_version.load(Ordering::Relaxed) == self.heap().allocation_version() {
            let cells = (self.first_cell()..self.end()).step_by(self.cell_size());
            return cells.for_each(visit);
        }
        for_each_set_bit(&self.live_bits(), |atom| {
            visit(self.address() + atom * ATOM_SIZE);
        });
    }

    /// Those that are marked when it starts. The marks are current.
    pub(super) fn for_each_marked_cell(&self, mut visit: impl FnMut(usize)) {
        for_each_set_bit(&self.marks.snapshot(), |atom| {
            visit(self.address() + atom * ATOM_SIZE);
        });
    }

    /// In the order of their addresses.
    #[inline(always)]
    fn for_each_dead_run(&self, live: &Bits, mut visit: impl FnMut(usize, usize)) {
        let (mut start, cell_size) = (self.first_cell(), self.cell_size());
        for_each_set_bit(live, |atom| {
            let cell = self.address() + atom * ATOM_SIZE;
            if cell != start {
                visit(start, cell);
            }
            start = cell + cell_size;
        });
        if start != self.end() {
            visit(start, self.end());
        }
    }

    /// Whether this is the first request since the block was last empty.
    #[inline(always)]
    pub(super) fn request_destruction(&self) -> bool {
        !self.is_destruction_requested.replace(true)
    }

    pub(super) fn is_destruction_requested(&self) -> bool {
        self.is_destruction_requested.get()
    }

    /// # Safety
    /// On the thread of the heap, which is in the sweeping state.
    pub(super) unsafe fn destroy_dead_cells(&self) {
        debug_assert!(!self.is_free_listed.get());
        let live = self.live_bits();
        // SAFETY: the caller's. Each is how the cells of such a block are destroyed.
        unsafe {
            match (self.attributes.destruction, self.attributes.destroy) {
                (_, None) => {}
                (Destruction::ByFunction, Some(destroy)) => {
                    self.destroy_with(&live, |cell| destroy(cell), |_| {});
                }
                (Destruction::OfStrings | Destruction::OnRequest, _) => self.destroy_with(
                    &live,
                    |cell| cell.cast::<JsString>().drop_in_place(),
                    |cell| {
                        prefetch(
                            cell.byte_add(JsString::OFFSET_OF_FIBER)
                                .cast::<usize>()
                                .read(),
                        )
                    },
                ),
            }
        }
        if live == Bits::default() {
            self.virgin_offset.set(self.first_atom * ATOM_SIZE as u32);
            self.is_destruction_requested.set(false);
        }
    }

    /// `prepare` gets a cell, which may be alive or destroyed, some time before its turn comes.
    ///
    /// # Safety
    /// As for `destroy_dead_cells`.
    #[inline(always)]
    unsafe fn destroy_with(
        &self,
        live: &Bits,
        destroy: impl Fn(*mut JsCell),
        prepare: impl Fn(*const JsCell),
    ) {
        const LOOKAHEAD: usize = 8;
        let touched_end = self.address() + self.virgin_offset.get() as usize;
        let (cell_size, discarded) = (self.cell_size(), self.discarded_pages.get());
        self.for_each_dead_run(live, |from, to| {
            for cell in (from..to.min(touched_end)).step_by(cell_size) {
                if discarded != 0 && discarded >> (cell % BLOCK_SIZE / page_size()) & 1 != 0 {
                    continue;
                }
                let ahead = cell + LOOKAHEAD * cell_size;
                if ahead < touched_end && discarded == 0 {
                    prepare(core::ptr::with_exposed_provenance(ahead));
                }
                let header = core::ptr::with_exposed_provenance_mut::<u32>(cell);
                // SAFETY: a dead cell whose header is not zapped has not been destroyed.
                unsafe {
                    if *header != 0 {
                        destroy(header.cast());
                        *header = 0;
                    }
                }
            }
        });
    }

    /// Chains the dead cells in the order of their addresses.
    ///
    /// # Safety
    /// On the thread of the heap, and those that need it have been destroyed.
    pub(super) unsafe fn build_free_list(&self, secret: u64) -> FreeList {
        debug_assert!(!self.is_free_listed.get());
        self.reuse_discarded_pages();
        let (mut list, mut last) = (FreeList::default(), None);
        let scribbles = options().scribble_free_cells;
        self.for_each_dead_run(&self.live_bits(), |from, to| {
            // SAFETY: free cells.
            unsafe {
                if scribbles {
                    scribble(from, to, self.cell_size());
                }
                match last.replace((from, to - from)) {
                    Some((start, length)) => FreeInterval::write(start, length, from, secret),
                    None => list.first_interval = from,
                }
            }
            list.bytes += to - from;
        });
        if let Some((start, length)) = last {
            // SAFETY: as above.
            unsafe { FreeInterval::write(start, length, 0, secret) };
            self.is_free_listed.set(true);
        }
        list
    }

    /// # Safety
    /// As for `build_free_list`.
    pub(super) unsafe fn scribble_on_dead_cells(&self) {
        self.reuse_discarded_pages();
        self.for_each_dead_run(&self.live_bits(), |from, to| {
            // SAFETY: the caller's.
            unsafe { scribble(from, to, self.cell_size()) };
        });
    }

    /// Gives the pages that have no live cell back to the system.
    ///
    /// # Safety
    /// As for `build_free_list`.
    pub(super) unsafe fn discard_unused_pages(&self) {
        let page = page_size();
        self.for_each_dead_run(&self.live_bits(), |from, to| {
            let (first, end) = (from.next_multiple_of(page), to / page * page);
            if first >= end {
                return;
            }
            let (count, shift) = ((end - first) / page, first % BLOCK_SIZE / page);
            let pages = ((1 << count) - 1) << shift;
            if self.discarded_pages.get() & pages != pages {
                // SAFETY: whole pages of dead cells.
                unsafe { virtual_memory::discard(first, end - first) };
                self.discarded_pages.update(|discarded| discarded | pages);
            }
        });
    }

    #[inline]
    pub(super) fn reuse_discarded_pages(&self) {
        let discarded = self.discarded_pages.take();
        if discarded == 0 {
            return;
        }
        let page = page_size();
        for index in (0..BLOCK_SIZE / page).filter(|index| discarded >> index & 1 != 0) {
            // SAFETY: discarded above.
            unsafe { virtual_memory::reuse(self.address() + index * page, page) };
        }
    }

    pub(super) fn did_consume_free_list(&self) {
        let _lock = self.lock.lock_guard();
        self.is_free_listed.set(false);
        self.virgin_offset.set(BLOCK_SIZE as u32);
        self.allocated_version
            .store(self.heap().allocation_version(), Ordering::Relaxed);
    }

    /// `position` is where the next cell would have been allocated, and `free` are the ranges that are left.
    pub(super) fn stop_allocating(
        &self,
        position: usize,
        free: impl Iterator<Item = (usize, usize)>,
    ) {
        let _lock = self.lock.lock_guard();
        self.is_free_listed.set(false);
        let offset = (position - self.address()) as u32;
        self.virgin_offset.set(self.virgin_offset.get().max(offset));
        let (mut live, mut atom) = (Bits::default(), self.first_atom as usize);
        let atom_of = |address: usize| (address - self.address()) / ATOM_SIZE;
        for (free_start, free_end) in free.chain([(self.end(), self.end())]) {
            while atom < atom_of(free_start) {
                live[atom / 64 % BITMAP_WORDS] |= 1 << (atom % 64);
                atom += self.atoms_per_cell as usize;
            }
            atom = atom_of(free_end);
        }
        self.newly_allocated.assign(&live);
        self.newly_allocated_version
            .store(self.heap().allocation_version(), Ordering::Relaxed);
    }

    /// From now on every cell is dead.
    pub(super) fn forget_liveness(&self) {
        self.marking_version.store(NULL_VERSION, Ordering::Relaxed);
        self.forget_allocation_versions();
    }

    /// Before the versions start over.
    pub(super) fn forget_marks_unless_of(&self, version: Version) {
        if !self.has_marks_of(version) {
            self.marking_version.store(NULL_VERSION, Ordering::Relaxed);
        }
    }

    /// As above.
    pub(super) fn forget_allocation_versions(&self) {
        self.allocated_version
            .store(NULL_VERSION, Ordering::Relaxed);
        self.newly_allocated_version
            .store(NULL_VERSION, Ordering::Relaxed);
    }
}

/// The headers stay zapped.
///
/// # Safety
/// The range consists of free cells of that size.
#[cold]
unsafe fn scribble(from: usize, to: usize, cell_size: usize) {
    const POISON: u64 = 0xbadb_eef0;
    for cell in (from..to).step_by(cell_size) {
        let words = core::ptr::with_exposed_provenance_mut::<u64>(cell);
        // SAFETY: the caller's.
        unsafe {
            words.write(0);
            (1..cell_size / 8).for_each(|index| words.add(index).write(POISON));
        }
    }
}
