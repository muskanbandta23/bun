//! Spreads the mark stack over several threads.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bun_threading::{Condition, Guarded};

use super::block::Version;
use super::visitor::{Findings, Visitor};
use super::{Heap, HeapAnalyzer, Scope};
use crate::FastHashSet;

/// A small collection is over before a helper has woken up.
const VISITS_BEFORE_SHARING: usize = 2048;
const VISITS_BETWEEN_DONATIONS: usize = 128;
const SMALLEST_DONATION: usize = 16;
const LARGEST_SHARE: usize = 1024;

#[derive(Default)]
struct Shared {
    work: Vec<Vec<usize>>,
    active: usize,
    helpers: usize,
    is_done: bool,
    findings: Findings,
}

/// The state of one collection that all markers see.
pub(super) struct Marking {
    heap: *const Heap,
    pub(super) version: Version,
    pub(super) scope: Scope,
    pub(super) is_concurrent: bool,
    pub(super) analyzer: *mut HeapAnalyzer,
    pub(super) raced: Guarded<Vec<usize>>,
    marker_count: usize,
    shared: Guarded<Shared>,
    changed: Condition,
    waiting: AtomicUsize,
    has_shared_work: AtomicBool,
}

impl Marking {
    pub(super) fn new(heap: &Heap, scope: Scope, marker_count: usize) -> Marking {
        let analyzer = heap.active_analyzer();
        Marking {
            heap,
            version: heap.marking_version(),
            scope,
            is_concurrent: false,
            analyzer,
            raced: Guarded::default(),
            marker_count: if analyzer.is_null() { marker_count } else { 1 },
            shared: Guarded::default(),
            changed: Condition::new(),
            waiting: AtomicUsize::new(0),
            has_shared_work: AtomicBool::new(false),
        }
    }

    pub(super) fn opaque_roots(&self) -> &Guarded<FastHashSet<usize>> {
        // SAFETY: the heap outlives the collection.
        unsafe { &(*self.heap).opaque_roots }
    }

    /// Visits until nothing is left. What the helpers found is then in the findings of `visitor`.
    pub(super) fn drain(&self, visitor: &mut Visitor) {
        let budget = if self.marker_count > 1 {
            VISITS_BEFORE_SHARING
        } else {
            usize::MAX
        };
        drain_at_most(visitor, budget);
        if !visitor.stack.is_empty() {
            self.drain_in_parallel(visitor);
        }
        visitor.did_finish_draining();
    }

    fn drain_in_parallel(&self, visitor: &mut Visitor) {
        let Some(job) = pool::start(self, self.marker_count - 1) else {
            return drain_at_most(visitor, usize::MAX);
        };
        self.work(visitor);
        drop(job);
        let mut shared = self.shared.lock();
        while shared.helpers != 0 {
            self.changed.wait_guarded(&mut shared);
        }
        shared.is_done = false;
        visitor.findings.merge(&mut shared.findings);
    }

    fn will_start_job(&self) {
        self.shared.lock().active = 1;
    }

    fn did_join(&self) {
        let mut shared = self.shared.lock();
        shared.helpers += 1;
        shared.active += 1;
    }

    fn help(&self) {
        let mut visitor = Visitor::new(self, Vec::new());
        self.work(&mut visitor);
        let mut shared = self.shared.lock();
        shared.findings.merge(&mut visitor.findings);
        shared.helpers -= 1;
        self.changed.broadcast();
    }

    /// The caller counts as active.
    fn work(&self, visitor: &mut Visitor) {
        loop {
            while !visitor.stack.is_empty() {
                drain_at_most(visitor, VISITS_BETWEEN_DONATIONS);
                self.donate(visitor);
            }
            let mut shared = self.shared.lock();
            shared.active -= 1;
            loop {
                if let Some(work) = shared.work.pop() {
                    self.has_shared_work
                        .store(!shared.work.is_empty(), Ordering::Relaxed);
                    visitor.stack = work;
                    shared.active += 1;
                    break;
                }
                if shared.is_done || shared.active == 0 {
                    shared.is_done = true;
                    self.changed.broadcast();
                    return;
                }
                self.waiting.fetch_add(1, Ordering::Relaxed);
                self.changed.wait_guarded(&mut shared);
                self.waiting.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    fn donate(&self, visitor: &mut Visitor) {
        let waiting = self.waiting.load(Ordering::Relaxed);
        if waiting == 0
            || visitor.stack.len() < SMALLEST_DONATION * 2
            || self.has_shared_work.load(Ordering::Relaxed)
        {
            return;
        }
        let share = (visitor.stack.len() / (waiting + 1)).clamp(SMALLEST_DONATION, LARGEST_SHARE);
        let mut shared = self.shared.lock();
        for _ in 0..waiting {
            let Some(rest) = visitor.stack.len().checked_sub(share).filter(|&it| it > 0) else {
                break;
            };
            shared.work.push(visitor.stack.split_off(rest));
        }
        self.has_shared_work.store(true, Ordering::Relaxed);
        self.changed.broadcast();
    }
}

#[inline]
fn drain_at_most(visitor: &mut Visitor, mut budget: usize) {
    while budget != 0
        && let Some(entry) = visitor.stack.pop()
    {
        visitor.visit(entry);
        budget -= 1;
    }
}

/// The helper threads of the process, which serve one collection at a time.
mod pool {
    use bun_threading::{Condition, Guarded};

    use super::Marking;

    struct State {
        job: *const Marking,
        openings: usize,
        threads: usize,
    }

    // SAFETY: `Marking` synchronizes what the markers share, and waits for them before it goes.
    unsafe impl Send for State {}

    static STATE: Guarded<State> = Guarded::new(State {
        job: core::ptr::null(),
        openings: 0,
        threads: 0,
    });
    static JOB_POSTED: Condition = Condition::new();

    /// No helper joins once this is dropped.
    pub(super) struct Job;

    impl Drop for Job {
        fn drop(&mut self) {
            let mut state = STATE.lock();
            state.job = core::ptr::null();
            state.openings = 0;
        }
    }

    pub(super) fn start(marking: &Marking, helpers: usize) -> Option<Job> {
        const STACK_SIZE: usize = 1 << 20;
        let mut state = STATE.lock();
        if !state.job.is_null() {
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
        marking.will_start_job();
        state.job = marking;
        state.openings = helpers;
        JOB_POSTED.broadcast();
        Some(Job)
    }

    fn run() {
        loop {
            let mut state = STATE.lock();
            while state.openings == 0 {
                JOB_POSTED.wait_guarded(&mut state);
            }
            state.openings -= 1;
            // SAFETY: alive while it is posted, and then until this thread has left.
            let marking = unsafe { &*state.job };
            marking.did_join();
            drop(state);
            marking.help();
        }
    }
}
