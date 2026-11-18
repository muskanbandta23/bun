use core::sync::atomic::{AtomicU8, AtomicU64, Ordering, fence};

use super::{CellState, container};
use crate::JsCell;

pub const BLACK_THRESHOLD: u8 = CellState::PossiblyBlack as u8;
/// Above every state: each barrier takes the slow path, which has the fence.
pub const TAUTOLOGICAL_THRESHOLD: u8 = 100;

/// The highest threshold of all heaps, for code that has no `Vm` at hand.
static THRESHOLD: AtomicU8 = AtomicU8::new(BLACK_THRESHOLD);

#[inline(always)]
pub(crate) fn threshold() -> u8 {
    THRESHOLD.load(Ordering::Relaxed)
}

/// Whether a marker may be running beside this thread. It does not become true between two allocations.
#[inline(always)]
pub(crate) fn is_fenced() -> bool {
    threshold() != BLACK_THRESHOLD
}

/// Between initializing memory and storing a pointer to it.
#[inline(always)]
pub(crate) fn publish_fence() {
    if is_fenced() {
        fence(Ordering::Release);
    }
}

#[cold]
#[inline(never)]
pub fn slow_path(cell: &JsCell) {
    let heap = cell.heap();
    if is_fenced() {
        fence(Ordering::SeqCst);
        if cell.state() != BLACK_THRESHOLD {
            return;
        }
        // Black from an earlier collection and not reached yet by this one, which will see the store.
        if !container::of(cell.address()).is_marked(cell.address()) {
            return;
        }
    }
    heap.remember(cell);
}

/// As `memmove`, but a marker never sees half a word.
///
/// # Safety
/// Both are valid and aligned for `count` words that only this thread writes.
#[inline]
pub unsafe fn move_words(source: *const u64, target: *mut u64, count: usize) {
    if !is_fenced() {
        // SAFETY: the caller's, and no other thread is reading.
        return unsafe { core::ptr::copy(source, target, count) };
    }
    // SAFETY: the caller's.
    let copy = |index: usize| unsafe {
        AtomicU64::from_ptr(target.add(index)).store(*source.add(index), Ordering::Relaxed);
    };
    if target.addr() <= source.addr() {
        (0..count).for_each(copy);
    } else {
        (0..count).rev().for_each(copy);
    }
}

/// # Safety
/// As for `move_words`.
#[inline]
pub(crate) unsafe fn fill_words(target: *mut u64, count: usize, word: u64) {
    if !is_fenced() {
        // SAFETY: the caller's, and no other thread is reading.
        let words = unsafe {
            core::slice::from_raw_parts_mut(target.cast::<core::mem::MaybeUninit<u64>>(), count)
        };
        words.write_filled(word);
        return;
    }
    for index in 0..count {
        // SAFETY: the caller's.
        unsafe { AtomicU64::from_ptr(target.add(index)) }.store(word, Ordering::Relaxed);
    }
}
