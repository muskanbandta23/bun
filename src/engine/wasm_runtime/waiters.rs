//! Those who wait for a notification at an address in shared memory, in the order of their arrival.

use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use core::time::Duration;
use std::sync::Arc;
use std::time::Instant;

use bun_threading::{Condition, Guarded};

pub trait Waiter: Send + Sync {
    /// Called with a lock held, so it must not use this module.
    fn wake(&self);
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Expected {
    Bits32(u32),
    Bits64(u64),
}

/// As `memory.atomic.wait` numbers them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Waited {
    Woken,
    NotEqual,
    TimedOut,
}

type Queue = Guarded<Vec<(usize, Arc<dyn Waiter>)>>;

#[repr(align(64))]
struct Bucket(Queue);

const BUCKETS_LOG2: u32 = 6;

static BUCKETS: [Bucket; 1 << BUCKETS_LOG2] =
    [const { Bucket(Guarded::new(Vec::new())) }; 1 << BUCKETS_LOG2];

fn queue(address: usize) -> &'static Queue {
    let hash = address.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> (usize::BITS - BUCKETS_LOG2);
    &BUCKETS[hash].0
}

/// Gives `false` and does nothing if the memory has another value.
///
/// # Safety
/// The address is aligned and accessible for a value of that size.
pub unsafe fn enqueue(address: usize, expected: Expected, waiter: Arc<dyn Waiter>) -> bool {
    let mut queue = queue(address).lock();
    // SAFETY: the caller's.
    let is_equal = unsafe {
        match expected {
            Expected::Bits32(bits) => {
                let place = AtomicU32::from_ptr(ptr::with_exposed_provenance_mut(address));
                place.load(Ordering::SeqCst) == bits
            }
            Expected::Bits64(bits) => {
                let place = AtomicU64::from_ptr(ptr::with_exposed_provenance_mut(address));
                place.load(Ordering::SeqCst) == bits
            }
        }
    };
    if is_equal {
        queue.push((address, waiter));
    }
    is_equal
}

/// Gives `false` if it has been woken.
pub fn cancel(address: usize, waiter: &dyn Waiter) -> bool {
    let mut queue = queue(address).lock();
    let position = queue.iter().position(|(waits_at, other)| {
        *waits_at == address && ptr::addr_eq(Arc::as_ptr(other), waiter)
    });
    position.map(|position| queue.remove(position)).is_some()
}

/// Gives the number of those that were woken.
pub fn notify(address: usize, count: u32) -> u32 {
    let mut woken = 0;
    queue(address).lock().retain(|(waits_at, waiter)| {
        let wakes = *waits_at == address && woken < count;
        if wakes {
            waiter.wake();
            woken += 1;
        }
        !wakes
    });
    woken
}

/// Blocks a thread.
#[derive(Default)]
pub struct Parker {
    state: Guarded<Signals>,
    changed: Condition,
}

#[derive(Default)]
struct Signals {
    is_woken: bool,
    is_interrupted: bool,
}

impl Waiter for Parker {
    fn wake(&self) {
        self.state.lock().is_woken = true;
        self.changed.signal();
    }
}

impl Parker {
    /// For other threads. It takes effect during this wait or the next.
    pub fn interrupt(&self) {
        self.state.lock().is_interrupted = true;
        self.changed.signal();
    }

    /// `interrupted` decides whether the wait goes on.
    ///
    /// # Safety
    /// As for [`enqueue`]. No other thread waits with `self`.
    pub unsafe fn wait<E>(
        self: &Arc<Self>,
        address: usize,
        expected: Expected,
        timeout: Option<Duration>,
        mut interrupted: impl FnMut() -> Result<(), E>,
    ) -> Result<Waited, E> {
        let deadline = timeout.and_then(|timeout| Instant::now().checked_add(timeout));
        self.state.lock().is_woken = false;
        // SAFETY: the caller's.
        if !unsafe { enqueue(address, expected, Arc::<Self>::clone(self)) } {
            return Ok(Waited::NotEqual);
        }
        let mut state = self.state.lock();
        while !state.is_woken {
            if core::mem::take(&mut state.is_interrupted) {
                drop(state);
                interrupted().inspect_err(|_| {
                    cancel(address, &**self);
                })?;
                state = self.state.lock();
                continue;
            }
            let Some(deadline) = deadline else {
                self.changed.wait_guarded(&mut state);
                continue;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                drop(state);
                return Ok(match cancel(address, &**self) {
                    true => Waited::TimedOut,
                    false => Waited::Woken,
                });
            }
            let left = u64::try_from(left.as_nanos()).unwrap_or(u64::MAX);
            let _ = self.changed.timed_wait_guarded(&mut state, left);
        }
        Ok(Waited::Woken)
    }
}
