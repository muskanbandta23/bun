use core::cell::Cell;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use bun_threading::Mutex;

use super::directory::Directory;
use super::{CellAttributes, Heap};

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
pub(super) const FIRST_VERSION: Version = 2;

pub(super) fn next_version(version: Version) -> Version {
    version.checked_add(1).unwrap_or(FIRST_VERSION)
}

/// One bit for each atom. Only that of the first atom of a cell is used.
struct Bitmap([AtomicU64; BITMAP_WORDS]);

impl Bitmap {
    #[inline(always)]
    fn get(&self, atom: usize) -> bool {
        self.0[atom / 64 % BITMAP_WORDS].load(Ordering::Relaxed) >> (atom % 64) & 1 != 0
    }

    /// Whether it was set.
    #[inline(always)]
    fn test_and_set(&self, atom: usize) -> bool {
        let bit = 1 << (atom % 64);
        self.0[atom / 64 % BITMAP_WORDS].fetch_or(bit, Ordering::Relaxed) & bit != 0
    }

    #[inline(always)]
    fn assign_exclusively(&self, atom: usize, value: bool) {
        let word = &self.0[atom / 64 % BITMAP_WORDS];
        let cleared = word.load(Ordering::Relaxed) & !(1 << (atom % 64));
        word.store(cleared | u64::from(value) << (atom % 64), Ordering::Relaxed);
    }

    fn clear(&self) {
        for word in &self.0 {
            word.store(0, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> [u64; BITMAP_WORDS] {
        self.0.each_ref().map(|word| word.load(Ordering::Relaxed))
    }

    fn assign(&self, words: &[u64; BITMAP_WORDS]) {
        for (word, &value) in self.0.iter().zip(words) {
            word.store(value, Ordering::Relaxed);
        }
    }

    fn count(&self) -> usize {
        self.snapshot()
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }
}

/// The header of an aligned run of memory that is cut into cells of one size.
#[repr(C)]
pub struct Block {
    heap: *const Heap,
    marking_version: AtomicU32,
    mark_count: AtomicU32,
    marks: Bitmap,
    directory: *const Directory,
    pub(super) attributes: CellAttributes,
    atoms_per_cell: u32,
    first_atom: u32,
    reciprocal: u32,
    retire_count: u32,
    pub(super) index: Cell<u32>,
    /// The cells from this offset on have never been allocated, and their memory has not been touched.
    virgin_offset: Cell<u32>,
    /// Current: every cell has been allocated or has survived since the last collection.
    allocated_version: AtomicU32,
    /// Current: `newly_allocated` says which cells are live.
    newly_allocated_version: AtomicU32,
    is_free_listed: Cell<bool>,
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

pub(super) struct SweepResult {
    /// Zero if no cell is free, or if no list was asked for.
    pub(super) first_interval: usize,
    pub(super) free_bytes: usize,
    pub(super) is_empty: bool,
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
                mark_count: AtomicU32::new(0),
                marks: empty(),
                directory,
                attributes: directory.attributes(),
                atoms_per_cell: atoms_per_cell as u32,
                first_atom: first_atom as u32,
                reciprocal: (1u32 << RECIPROCAL_SHIFT).div_ceil(atoms_per_cell as u32),
                retire_count: (cells * 9).div_ceil(10) as u32,
                index: Cell::new(0),
                virgin_offset: Cell::new((first_atom * ATOM_SIZE) as u32),
                allocated_version: AtomicU32::new(NULL_VERSION),
                newly_allocated_version: AtomicU32::new(NULL_VERSION),
                is_free_listed: Cell::new(false),
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

    pub(super) fn first_cell(&self) -> usize {
        self.address() + self.first_atom as usize * ATOM_SIZE
    }

    /// The start of the cell that an address of this block is in.
    #[inline(always)]
    pub(super) fn cell_containing(&self, address: usize) -> Option<usize> {
        let atom = (Self::atom_of(address) as u32).checked_sub(self.first_atom)?;
        let index = (atom * self.reciprocal) >> RECIPROCAL_SHIFT;
        let first_atom = self.first_atom + index * self.atoms_per_cell;
        Some(self.address() + first_atom as usize * ATOM_SIZE)
    }

    pub(super) fn cells(&self) -> impl DoubleEndedIterator<Item = usize> + use<> {
        (self.first_cell()..self.address() + BLOCK_SIZE).step_by(self.cell_size())
    }

    #[inline(always)]
    pub(super) fn is_marked(&self, version: Version, cell: usize) -> bool {
        self.marking_version.load(Ordering::Acquire) == version
            && self.marks.get(Self::atom_of(cell))
    }

    /// Whether the cell was marked already.
    #[inline(always)]
    pub(super) fn test_and_set_marked(&self, version: Version, cell: usize) -> bool {
        if self.marking_version.load(Ordering::Acquire) != version {
            self.prepare_for_marking(version);
        }
        let atom = Self::atom_of(cell);
        if self.marks.get(atom) || self.marks.test_and_set(atom) {
            return true;
        }
        // Markers may lose each other's increments: the count only decides whether sweeping is worthwhile.
        let count = self.mark_count.load(Ordering::Relaxed) + 1;
        self.mark_count.store(count, Ordering::Relaxed);
        if count == self.retire_count {
            self.directory().did_retire(self);
        }
        false
    }

    #[cold]
    #[inline(never)]
    fn prepare_for_marking(&self, version: Version) {
        let _lock = self.lock.lock_guard();
        let old_version = self.marking_version.load(Ordering::Relaxed);
        if old_version == version {
            return;
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
        self.marks.clear();
        self.mark_count.store(0, Ordering::Relaxed);
        self.marking_version.store(version, Ordering::Release);
        self.directory().did_start_marking(self);
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
        self.marks_convey_liveness() && self.marks.get(Self::atom_of(cell))
    }

    pub(super) fn is_free_listed(&self) -> bool {
        self.is_free_listed.get()
    }

    pub(super) fn marked_cell_count(&self) -> usize {
        if self.marking_version.load(Ordering::Relaxed) == self.heap().marking_version() {
            self.marks.count()
        } else {
            0
        }
    }

    pub(super) fn has_touched_cells(&self) -> bool {
        self.address() + self.virgin_offset.get() as usize != self.first_cell()
    }

    fn live_bits(&self) -> [u64; BITMAP_WORDS] {
        let _lock = self.lock.lock_guard();
        let version = self.heap().allocation_version();
        debug_assert!(self.allocated_version.load(Ordering::Relaxed) != version);
        if self.newly_allocated_version.load(Ordering::Relaxed) == version {
            self.newly_allocated.snapshot()
        } else if self.marks_convey_liveness() {
            self.marks.snapshot()
        } else {
            [0; BITMAP_WORDS]
        }
    }

    /// Destroys the dead cells and, given a secret, chains the free ones in the order of their addresses.
    ///
    /// # Safety
    /// On the thread of the heap, which is in the sweeping state.
    pub(super) unsafe fn sweep(&self, secret: Option<u64>) -> SweepResult {
        debug_assert!(!self.is_free_listed.get());
        let live = self.live_bits();
        let is_empty = live.iter().all(|&word| word == 0);
        let is_live = |cell: usize| {
            let atom = Self::atom_of(cell);
            live[atom / 64 % BITMAP_WORDS] >> (atom % 64) & 1 != 0
        };
        let (start, end) = (self.first_cell(), self.address() + BLOCK_SIZE);

        if let Some(destroy) = self.attributes.destroy {
            let touched_end = self.address() + self.virgin_offset.get() as usize;
            for cell in (start..touched_end).step_by(self.cell_size()) {
                let header = core::ptr::with_exposed_provenance_mut::<u32>(cell);
                // SAFETY: a dead cell whose header is not zapped has not been destroyed.
                unsafe {
                    if !is_live(cell) && *header != 0 {
                        destroy(header.cast());
                        *header = 0;
                    }
                }
            }
        }
        if is_empty {
            self.virgin_offset.set((start - self.address()) as u32);
        }
        let Some(secret) = secret else {
            return SweepResult {
                first_interval: 0,
                free_bytes: 0,
                is_empty,
            };
        };

        let (mut next, mut free_bytes) = (0, 0);
        let mut add_interval = |from: usize, to: usize| {
            if from != to {
                #[cfg(debug_assertions)]
                // SAFETY: free cells. Their headers stay zapped.
                unsafe {
                    scribble(from, to, self.cell_size());
                }
                // SAFETY: a free cell.
                unsafe { FreeInterval::write(from, to - from, next, secret) };
                next = from;
                free_bytes += to - from;
            }
        };
        if is_empty {
            add_interval(start, end);
        } else {
            let mut interval_end = end;
            for cell in self.cells().rev() {
                if is_live(cell) {
                    add_interval(cell + self.cell_size(), interval_end);
                    interval_end = cell;
                }
            }
            add_interval(start, interval_end);
        }
        self.is_free_listed.set(next != 0);
        SweepResult {
            first_interval: next,
            free_bytes,
            is_empty,
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
        self.newly_allocated.clear();
        let mut cell = self.first_cell();
        let end = self.address() + BLOCK_SIZE;
        for (free_start, free_end) in free.chain([(end, end)]) {
            while cell < free_start {
                self.newly_allocated
                    .assign_exclusively(Self::atom_of(cell), true);
                cell += self.cell_size();
            }
            cell = free_end;
        }
        self.newly_allocated_version
            .store(self.heap().allocation_version(), Ordering::Relaxed);
    }

    /// Makes the next sweep destroy every cell.
    pub(super) fn forget_liveness(&self) {
        self.marking_version.store(NULL_VERSION, Ordering::Relaxed);
        self.forget_allocation_versions();
    }

    pub(super) fn forget_allocation_versions(&self) {
        self.allocated_version
            .store(NULL_VERSION, Ordering::Relaxed);
        self.newly_allocated_version
            .store(NULL_VERSION, Ordering::Relaxed);
    }
}

/// # Safety
/// The range consists of free cells of that size.
#[cfg(debug_assertions)]
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
