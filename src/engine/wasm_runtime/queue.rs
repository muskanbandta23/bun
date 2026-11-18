//! Threads that validate and compile in the background. They start on demand and end when they are idle.

use std::collections::VecDeque;
use std::sync::LazyLock;

use bun_engine_wasm_format::Scratch;
use bun_threading::{Condition, Guarded};

use crate::code::Compilers;

/// The most urgent first.
#[derive(Clone, Copy)]
pub(crate) enum Priority {
    Validation,
    LoopEntry,
    Optimization,
}

const PRIORITIES: usize = 3;
const STACK_SIZE: usize = 4 << 20;
const IDLE_NANOSECONDS: u64 = 10_000_000_000;

/// Memory that one job leaves to the next.
#[derive(Default)]
pub(crate) struct Workspace {
    pub(crate) validation: Scratch,
    pub(crate) compilers: Compilers,
}

type Job = Box<dyn FnOnce(&mut Workspace) + Send>;

struct State {
    jobs: [VecDeque<Job>; PRIORITIES],
    threads: usize,
    /// Of those, how many run a job.
    busy: usize,
}

impl State {
    fn waiting_jobs(&self) -> usize {
        self.jobs.iter().map(VecDeque::len).sum()
    }
}

static STATE: Guarded<State> = Guarded::new(State {
    jobs: [const { VecDeque::new() }; PRIORITIES],
    threads: 0,
    busy: 0,
});
static JOB_ADDED: Condition = Condition::new();
static ALL_DONE: Condition = Condition::new();

pub(crate) fn max_threads() -> usize {
    static COUNT: LazyLock<usize> = LazyLock::new(|| {
        let processors = std::thread::available_parallelism().map_or(1, usize::from);
        (processors - 1).clamp(1, 16)
    });
    *COUNT
}

/// If there is no thread and none can be started, the job runs on this one.
pub(crate) fn schedule(priority: Priority, job: impl FnOnce(&mut Workspace) + Send + 'static) {
    let mut state = STATE.lock();
    state.jobs[priority as usize].push_back(Box::new(job));
    let needs_thread =
        state.threads < max_threads() && state.threads - state.busy < state.waiting_jobs();
    state.threads += usize::from(needs_thread);
    drop(state);
    JOB_ADDED.signal();
    if needs_thread {
        let thread = std::thread::Builder::new().name("Wasm Compiler".into());
        if thread
            .stack_size(STACK_SIZE)
            .spawn(|| work(IDLE_NANOSECONDS))
            .is_err()
        {
            stand_in();
        }
    }
}

#[cold]
fn stand_in() {
    let mut state = STATE.lock();
    if state.threads > 1 {
        state.threads -= 1;
        return;
    }
    drop(state);
    work(0);
}

fn work(idle_nanoseconds: u64) {
    let mut workspace = Workspace::default();
    let mut state = STATE.lock();
    loop {
        let Some(job) = state.jobs.iter_mut().find_map(VecDeque::pop_front) else {
            let waited = JOB_ADDED.timed_wait_guarded(&mut state, idle_nanoseconds);
            if waited.is_err() && state.waiting_jobs() == 0 {
                state.threads -= 1;
                return;
            }
            continue;
        };
        state.busy += 1;
        drop(state);
        job(&mut workspace);
        state = STATE.lock();
        state.busy -= 1;
        if state.busy == 0 && state.waiting_jobs() == 0 {
            ALL_DONE.broadcast();
        }
    }
}

/// Returns when the threads in the background have nothing to do.
pub fn wait_for_background_work() {
    let mut state = STATE.lock();
    while state.busy != 0 || state.waiting_jobs() != 0 {
        ALL_DONE.wait_guarded(&mut state);
    }
}
