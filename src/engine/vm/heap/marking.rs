//! Spreads the mark stack over several threads.

use core::cell::{Cell, UnsafeCell};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use bun_threading::{Condition, Guarded};

use super::block::Version;
use super::visitor::{Findings, Visitor};
use super::{HeapAnalyzer, Scope};
use crate::FastHashSet;

/// A small collection is over before a helper has woken up.
const WORK_BEFORE_SHARING: usize = 4096;
const WORK_BETWEEN_DONATIONS: usize = 128;
/// How often a marker without work looks for some before it goes to sleep.
const SPINS: usize = 512;

/// Addresses outside the heap that marking has reached. They are added only while there is one marker.
#[derive(Default)]
pub(super) struct OpaqueRoots(UnsafeCell<FastHashSet<usize>>);

impl OpaqueRoots {
    #[inline]
    pub(super) fn contains(&self, root: usize) -> bool {
        // SAFETY: see `get_mut`.
        unsafe { &*self.0.get() }.contains(&root)
    }

    /// # Safety
    /// On the thread that collects, while no helper is at work. Nothing else is called until the borrow ends.
    #[expect(clippy::mut_from_ref, reason = "the caller is alone")]
    pub(super) unsafe fn get_mut(&self) -> &mut FastHashSet<usize> {
        // SAFETY: the caller's.
        unsafe { &mut *self.0.get() }
    }
}

/// The state of one collection that all markers see.
pub(super) struct Marking {
    pub(super) version: Version,
    pub(super) scope: Scope,
    pub(super) is_concurrent: bool,
    pub(super) analyzer: *mut HeapAnalyzer,
    pub(super) raced: Guarded<Vec<usize>>,
    opaque_roots: *const OpaqueRoots,
    helper_count: usize,
}

impl Marking {
    /// `opaque_roots` and `analyzer` outlive it.
    pub(super) fn new(
        version: Version,
        scope: Scope,
        opaque_roots: &OpaqueRoots,
        analyzer: *mut HeapAnalyzer,
        marker_count: usize,
    ) -> Marking {
        Marking {
            version,
            scope,
            is_concurrent: false,
            analyzer,
            raced: Guarded::default(),
            opaque_roots,
            helper_count: if analyzer.is_null() {
                marker_count - 1
            } else {
                0
            },
        }
    }

    pub(super) fn opaque_roots(&self) -> &OpaqueRoots {
        // SAFETY: see `new`.
        unsafe { &*self.opaque_roots }
    }

    /// Visits until nothing is left. What the helpers found is then in the findings of `visitor`.
    pub(super) fn drain(&self, visitor: &mut Visitor) {
        let mut work_alone = WORK_BEFORE_SHARING;
        while !visitor.stack.is_empty() {
            visitor.drain(work_alone);
            if visitor.stack.len() > 1 && self.helper_count != 0 {
                self.drain_with_helpers(visitor);
                work_alone = work_alone.saturating_mul(2);
            }
        }
        visitor.did_finish_draining();
    }

    /// Until nothing is left, or they have all left for want of work.
    fn drain_with_helpers(&self, visitor: &mut Visitor) {
        let job = Arc::new(Job::new(self));
        let Some(posting) = pool::post(&job, self.helper_count) else {
            return;
        };
        visitor.is_alone = false;
        job.work(visitor, Role::Collector);
        visitor.is_alone = true;
        drop(posting);
        visitor.findings.merge(&mut job.shared.lock().findings);
        // SAFETY: the job is done.
        let opaque_roots = unsafe { self.opaque_roots().get_mut() };
        opaque_roots.extend(visitor.findings.opaque_roots.drain());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Stays until the job is done.
    Collector,
    Helper,
}

#[derive(Default)]
struct Shared {
    shares: Vec<Vec<usize>>,
    active: usize,
    idle: usize,
    is_collector_asleep: bool,
    is_done: bool,
    findings: Findings,
}

/// Several markers at work. A helper may hold on to it for a while after it is done.
struct Job {
    /// Alive until the job is done.
    marking: *const Marking,
    helper_count: usize,
    shared: Guarded<Shared>,
    changed: Condition,
    /// The markers that look for work, less the shares that wait for them.
    hungry: AtomicUsize,
    /// The helpers that have given up looking.
    absent: AtomicUsize,
    /// There is a share to take, or the job is done.
    has_news: AtomicBool,
}

// SAFETY: what markers use of the `Marking` does not change, or is behind a lock.
unsafe impl Send for Job {}
// SAFETY: as above.
unsafe impl Sync for Job {}

impl Job {
    fn new(marking: &Marking) -> Job {
        Job {
            marking,
            helper_count: marking.helper_count,
            shared: Guarded::new(Shared {
                active: 1,
                ..Shared::default()
            }),
            changed: Condition::new(),
            hungry: AtomicUsize::new(0),
            absent: AtomicUsize::new(0),
            has_news: AtomicBool::new(false),
        }
    }

    fn help(&self) {
        let mut shared = self.shared.lock();
        if shared.is_done {
            return;
        }
        shared.active += 1;
        drop(shared);
        // SAFETY: the job is not done while a marker is active.
        let mut visitor = Visitor::new(unsafe { &*self.marking }, Vec::new());
        visitor.is_alone = false;
        self.work(&mut visitor, Role::Helper);
    }

    /// The caller counts as active.
    fn work(&self, visitor: &mut Visitor, role: Role) {
        const TURNS_BETWEEN_RECALLS: usize = WORK_BEFORE_SHARING / WORK_BETWEEN_DONATIONS;
        loop {
            let mut turns = 0;
            while !visitor.stack.is_empty() {
                visitor.drain(WORK_BETWEEN_DONATIONS);
                turns += 1;
                let absent = self.absent.load(Ordering::Relaxed);
                if absent == self.helper_count && self.end_if_alone() {
                    return;
                }
                if visitor.stack.len() < 2 {
                    continue;
                }
                if self.hungry.load(Ordering::Relaxed) != 0 {
                    self.donate(&mut visitor.stack);
                } else if absent != 0 && turns >= TURNS_BETWEEN_RECALLS {
                    turns = 0;
                    self.recall_helper(&mut visitor.stack);
                }
            }
            match self.wait_for_share(&mut visitor.findings, role) {
                Some(share) => visitor.stack = share,
                None => return,
            }
        }
    }

    /// All helpers have been seen absent, so the caller is the collector unless one has been recalled since.
    #[cold]
    fn end_if_alone(&self) -> bool {
        let mut shared = self.shared.lock();
        shared.is_done =
            self.absent.load(Ordering::Relaxed) == self.helper_count && shared.shares.is_empty();
        shared.is_done
    }

    fn update_hints(&self, shared: &Shared) {
        let hungry = shared.idle.saturating_sub(shared.shares.len());
        self.hungry.store(hungry, Ordering::Relaxed);
        let has_news = !shared.shares.is_empty() || shared.is_done;
        self.has_news.store(has_news, Ordering::Relaxed);
        if has_news && shared.is_collector_asleep {
            self.changed.signal();
        }
    }

    /// `None` once the caller has nothing more to do with the job.
    fn wait_for_share(&self, findings: &mut Findings, role: Role) -> Option<Vec<usize>> {
        let mut shared = self.shared.lock();
        if role == Role::Helper {
            shared.findings.merge(findings);
        }
        shared.active -= 1;
        shared.idle += 1;
        shared.is_done = shared.active == 0 && shared.shares.is_empty();
        let mut has_waited = false;
        while shared.shares.is_empty() && !shared.is_done {
            if !has_waited {
                self.update_hints(&shared);
                drop(shared);
                has_waited = !(0..SPINS).any(|_| {
                    core::hint::spin_loop();
                    self.has_news.load(Ordering::Relaxed)
                });
                shared = self.shared.lock();
            } else if role == Role::Helper {
                self.absent.fetch_add(1, Ordering::Relaxed);
                break;
            } else {
                shared.is_collector_asleep = true;
                self.changed.wait_guarded(&mut shared);
                shared.is_collector_asleep = false;
            }
        }
        let share = shared.shares.pop();
        shared.idle -= 1;
        shared.active += usize::from(share.is_some());
        self.update_hints(&shared);
        share
    }

    /// Half of the stack. What is at the bottom has the most behind it, but is expensive to get at in a deep one.
    #[inline(never)]
    fn donate(&self, stack: &mut Vec<usize>) {
        const DEEP: usize = 4096;
        let share = if stack.len() > DEEP {
            stack.split_off(stack.len() - DEEP / 2)
        } else {
            stack.drain(..stack.len() / 2).collect()
        };
        let mut shared = self.shared.lock();
        shared.shares.push(share);
        self.update_hints(&shared);
    }

    #[cold]
    fn recall_helper(&self, stack: &mut Vec<usize>) {
        let shared = self.shared.lock();
        let Some(absent) = self.absent.load(Ordering::Relaxed).checked_sub(1) else {
            return;
        };
        self.absent.store(absent, Ordering::Relaxed);
        drop(shared);
        self.donate(stack);
        pool::add_opening();
    }
}

#[thread_local]
static IS_HELPER_THREAD: Cell<bool> = Cell::new(false);

pub fn is_helper_thread() -> bool {
    IS_HELPER_THREAD.get()
}

/// The helper threads of the process, which serve one collection at a time.
mod pool {
    use std::sync::Arc;

    use bun_threading::{Condition, Guarded};

    use super::Job;

    struct State {
        job: Option<Arc<Job>>,
        openings: usize,
        threads: usize,
    }

    static STATE: Guarded<State> = Guarded::new(State {
        job: None,
        openings: 0,
        threads: 0,
    });
    static OPENING_ADDED: Condition = Condition::new();

    /// No helper joins once this is dropped.
    pub(super) struct Posting;

    impl Drop for Posting {
        fn drop(&mut self) {
            let mut state = STATE.lock();
            state.job = None;
            state.openings = 0;
        }
    }

    /// `None` if they are busy.
    pub(super) fn post(job: &Arc<Job>, helpers: usize) -> Option<Posting> {
        const STACK_SIZE: usize = 1 << 20;
        let mut state = STATE.lock();
        if state.job.is_some() {
            return None;
        }
        while state.threads < helpers {
            let thread = std::thread::Builder::new()
                .name("HeapHelper".into())
                .stack_size(STACK_SIZE)
                .spawn(run);
            if thread.is_err() {
                break;
            }
            state.threads += 1;
        }
        state.job = Some(Arc::clone(job));
        state.openings = helpers;
        drop(state);
        OPENING_ADDED.signal();
        Some(Posting)
    }

    /// By an active marker of the job that is posted.
    pub(super) fn add_opening() {
        STATE.lock().openings += 1;
        OPENING_ADDED.signal();
    }

    fn run() {
        const IDLE_NANOSECONDS: u64 = 10_000_000_000;
        super::IS_HELPER_THREAD.set(true);
        loop {
            let mut state = STATE.lock();
            while state.openings == 0 {
                let result = OPENING_ADDED.timed_wait_guarded(&mut state, IDLE_NANOSECONDS);
                if result.is_err() && state.openings == 0 {
                    state.threads -= 1;
                    return;
                }
            }
            state.openings -= 1;
            let (job, openings) = (state.job.clone(), state.openings);
            drop(state);
            // The thread that collects has better things to do than to wake them all.
            for _ in 0..openings.min(2) {
                OPENING_ADDED.signal();
            }
            job.expect("posted").help();
        }
    }
}
