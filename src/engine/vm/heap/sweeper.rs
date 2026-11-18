//! Destroys what has died and gives memory back, where allocation does not get to it first.

use core::cell::Cell;

use bun_engine_platform::clock;

use super::activity::HeapTimer;
use super::collector::log;
use super::directory::Directory;
use super::{BLOCK_SIZE, Heap, MutatorState};
use crate::options;

const SLICE_SECONDS: f64 = 0.01;
const SECONDS_BETWEEN_SLICES: f64 = 0.1;
/// The empty blocks that are kept are those that it takes to go on allocating at the same pace for that long.
const RESERVE_SECONDS: f64 = 1.0;

#[derive(Default)]
pub(super) struct Sweeper {
    timer: HeapTimer,
    unswept_cursor: Cell<usize>,
    sparse_cursor: Cell<usize>,
    last_slice_time: Cell<f64>,
    allocated_at_last_slice: Cell<usize>,
}

impl Heap {
    /// False once `step` has said so of every directory.
    fn step_through(&self, cursor: &Cell<usize>, step: impl Fn(&Directory) -> bool) -> bool {
        loop {
            let Some(&allocator) = self.allocators.borrow().get(cursor.get()) else {
                return false;
            };
            if allocator.directory().is_some_and(&step) {
                return true;
            }
            cursor.update(|index| index + 1);
        }
    }

    fn sweep_next_block(&self) -> bool {
        self.step_through(&self.sweeper.unswept_cursor, Directory::sweep_next_block)
    }

    fn discard_pages_of_next_sparse_block(&self) -> bool {
        let step = Directory::discard_pages_of_next_sparse_block;
        options().discard_unused_pages && self.step_through(&self.sweeper.sparse_cursor, step)
    }

    fn empty_block_count(&self) -> usize {
        let mut count = 0;
        self.for_each_directory(|directory| count += directory.empty_block_count());
        count
    }

    /// As long as there are more than `keep` empty ones and `goes_on` says so.
    fn free_swept_empty_blocks_while(&self, keep: usize, goes_on: impl Fn() -> bool) {
        let mut excess = self.empty_block_count().saturating_sub(keep);
        self.for_each_directory(|directory| {
            while excess != 0
                && goes_on()
                && let Some(block) = directory.take_swept_empty_block()
            {
                self.blocks.borrow_mut().remove(block);
                // SAFETY: empty, and in no directory.
                unsafe { directory.allocator().source.free(block) };
                excess -= 1;
            }
        });
    }

    pub(super) fn free_swept_empty_blocks(&self, keep: usize) {
        self.free_swept_empty_blocks_while(keep, || true);
    }

    pub fn sweep_synchronously(&self) {
        let (start, before) = (clock::monotonic_seconds(), self.capacity());
        while self.sweep_next_block() {}
        self.free_swept_empty_blocks(0);
        if options().use_zombie_mode {
            self.for_each_directory(Directory::scribble_on_dead_cells);
        } else {
            while self.discard_pages_of_next_sparse_block() {}
        }
        self.sweeper.timer.cancel();
        let elapsed = (clock::monotonic_seconds() - start) * 1e3;
        let (before, after) = (before >> 10, self.capacity() >> 10);
        log!(self, "Full sweep: {before}kb => {after}kb, {elapsed:.3}ms]");
    }

    pub(super) fn reset_sweeper(&self) {
        self.sweeper.unswept_cursor.set(0);
        self.sweeper.sparse_cursor.set(0);
        self.sweeper.allocated_at_last_slice.set(0);
        (self.sweeper.last_slice_time).set(clock::monotonic_seconds());
    }

    pub(super) fn schedule_sweep(&self, seconds: Option<f64>) {
        let seconds = seconds.unwrap_or(SECONDS_BETWEEN_SLICES);
        (self.sweeper.timer).start(self.vm(), seconds, |vm| vm.heap.sweep_for_a_while());
    }

    fn sweep_for_a_while(&self) {
        if self.mutator_state() != MutatorState::Running {
            return self.schedule_sweep(None);
        }
        let now = clock::monotonic_seconds();
        let has_time = || clock::monotonic_seconds() < now + SLICE_SECONDS;
        let allocated = self.policy.allocated();
        let bytes = allocated - self.sweeper.allocated_at_last_slice.replace(allocated);
        let seconds = now - self.sweeper.last_slice_time.replace(now);
        let reserve = (bytes as f64 / seconds * RESERVE_SECONDS) as usize;
        let keep = reserve.min(self.policy.remaining()) / BLOCK_SIZE;

        while has_time() && self.sweep_next_block() {}
        self.free_swept_empty_blocks_while(keep, has_time);
        while has_time() && self.discard_pages_of_next_sparse_block() {}
        if !has_time() {
            self.schedule_sweep(None);
        } else if self.empty_block_count() != 0 {
            self.schedule_sweep(Some(RESERVE_SECONDS));
        }
    }
}
