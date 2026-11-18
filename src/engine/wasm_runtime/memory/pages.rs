//! The bytes of a linear memory.

use core::ops::Range;
use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use bun_threading::{Mutex, MutexGuard};

use super::budget::{ADDRESS_SPACE, COMMITTED, Need};
use super::system;

/// A range of addresses, of which the first `len` are accessible for as long as this lives.
pub struct Pages {
    base: usize,
    /// Reserved below the base.
    margin: u64,
    /// From the base up.
    reserved: u64,
    length: AtomicU64,
    growth: Mutex,
}

/// For memories without room for a single page. Nothing is mapped there.
const NOWHERE: usize = 8;

impl Pages {
    pub(super) fn reserve(size: u64, need: Need) -> Option<Self> {
        Self::reserve_with_margin(0, size, need)
    }

    pub(super) fn reserve_with_margin(margin: u64, size: u64, need: Need) -> Option<Self> {
        let total = margin + size;
        if !ADDRESS_SPACE.take(total, need) {
            return None;
        }
        let start = match total {
            0 => Some(NOWHERE),
            _ => system::reserve(total as usize),
        };
        let Some(start) = start else {
            ADDRESS_SPACE.give_back(total);
            return None;
        };
        Some(Self {
            base: start + margin as usize,
            margin,
            reserved: size,
            length: AtomicU64::new(0),
            growth: Mutex::new(),
        })
    }

    /// Other threads may write to the bytes of a shared memory at any time.
    #[inline]
    pub fn base(&self) -> *mut u8 {
        ptr::with_exposed_provenance_mut(self.base)
    }

    /// In bytes. It never shrinks.
    #[inline]
    pub fn len(&self) -> u64 {
        self.length.load(Ordering::Acquire)
    }

    pub(super) fn length_address(&self) -> usize {
        self.length.as_ptr().expose_provenance()
    }

    pub(super) fn reserved(&self) -> u64 {
        self.reserved
    }

    pub(super) fn reservation(&self) -> Range<usize> {
        self.base - self.margin as usize..self.base + self.reserved as usize
    }

    pub(super) fn lock_growth(&self) -> Growth<'_> {
        Growth {
            pages: self,
            _lock: self.growth.lock_guard(),
        }
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        let reservation = self.reservation();
        if !reservation.is_empty() {
            // SAFETY: the reservation that `reserve` made. Whoever accesses the bytes keeps this alive.
            unsafe { system::release(reservation.start, reservation.len()) };
        }
        COMMITTED.give_back(*self.length.get_mut());
        ADDRESS_SPACE.give_back(reservation.len() as u64);
    }
}

/// The length does not change but through this.
pub(super) struct Growth<'a> {
    pages: &'a Pages,
    _lock: MutexGuard,
}

impl Growth<'_> {
    /// `length` is a multiple of the size of a page, not less than the length so far, and within the reservation.
    pub(super) fn extend_to(&self, length: u64) -> bool {
        let old = self.pages.len();
        assert!(old <= length && length <= self.pages.reserved);
        let added = length - old;
        if added == 0 {
            return true;
        }
        if !COMMITTED.take(added, Need::Function) {
            return false;
        }
        // SAFETY: within the reservation. No system has pages larger than those of WebAssembly.
        if !unsafe { system::commit(self.pages.base + old as usize, added as usize) } {
            COMMITTED.give_back(added);
            return false;
        }
        self.pages.length.store(length, Ordering::Release);
        true
    }
}
