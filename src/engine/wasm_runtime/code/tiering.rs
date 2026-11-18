//! Decides when a function gets better code.
//!
//! The entry and each loop count for themselves and report after a part of the threshold. Once the sum reaches it, the
//! function is optimized. A frame that stays in a loop is taken over by code that starts there, preferably at an outer
//! loop, which is made to report the next time it turns.

use core::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use bun_engine_macros::native_abi;
use bun_engine_wasm_compiler::abi::MemoryMode;
use bun_engine_wasm_format::CodeIndex;
use smallvec::SmallVec;

use super::group::{Function, Optimized, State};
use super::installed::Kind;
use super::loops::Nesting;
use super::{CodeGroup, Compilers, InstalledCode, STACK_FOR_COMPILERS};
use crate::engine::{Engine, Tiering};
use crate::instance::Instance;
use crate::module::Module;
use crate::queue::{self, Priority};

const REPORTS_TO_THRESHOLD: u32 = 4;
const AT_ONCE: i32 = -1;
const NEVER: i32 = i32::MIN;
/// A function that was entered no more often gets its code when it is entered again.
const FEW_ENTRIES: u32 = 2;
const MAX_LOOP_ENTRIES: usize = 4;
/// How many reports of an inner loop an entry at an outer loop may take to be reached.
const PATIENCE: u8 = 4;

fn threshold(engine: &Engine) -> u32 {
    match engine.options().tiering {
        Tiering::After(threshold) => threshold,
        Tiering::BaselineOnly | Tiering::OptimizedOnly => 0,
    }
}

pub(super) fn first_countdown(engine: &Engine) -> i32 {
    -((threshold(engine) / REPORTS_TO_THRESHOLD).clamp(1, 1 << 30) as i32)
}

/// Less for small functions, which do little in a call.
pub(super) fn entry_increment(module: &Module, code: CodeIndex) -> i32 {
    const FULL: u32 = 15;
    const SIZE_FOR_FULL: u32 = 128;
    let size = module.body_span(code).len.min(SIZE_FOR_FULL);
    (FULL * size / SIZE_FOR_FULL).max(1) as i32
}

pub(super) struct Heat {
    /// What countdowns had counted when they were set again.
    reported: u32,
    is_hot: bool,
    reported_at_entry: u32,
    entry_start: i32,
    loops: Option<Box<Loops>>,
}

struct Loops {
    nesting: Nesting,
    /// The outermost that has turned since those around a hot loop were asked to report.
    candidate: Option<u32>,
    is_compiling: bool,
    /// Those for which an entry was asked for.
    attempted: SmallVec<[u32; MAX_LOOP_ENTRIES]>,
    impatience: u8,
}

fn counted(countdown: &AtomicI32, start: i32) -> u32 {
    countdown.load(Ordering::Relaxed).wrapping_sub(start).max(0) as u32
}

impl Heat {
    pub(super) fn new(entry_start: i32) -> Self {
        Self {
            reported: 0,
            is_hot: false,
            reported_at_entry: 0,
            entry_start,
            loops: None,
        }
    }

    pub(super) fn counted_at_entry(&self, countdown: &AtomicI32) -> u32 {
        let since = counted(countdown, self.entry_start);
        self.reported_at_entry.saturating_add(since)
    }

    fn set_entry(&mut self, countdown: &AtomicI32, value: i32) {
        self.reported_at_entry = self.counted_at_entry(countdown);
        self.entry_start = value;
        countdown.store(value, Ordering::Relaxed);
    }
}

/// A function whose baseline code reports.
struct Reporter<'a> {
    instance: &'a Instance,
    group: &'a CodeGroup,
    code: CodeIndex,
    function: &'a Function,
    /// From one report to the next.
    interval: i32,
}

impl<'a> Reporter<'a> {
    fn new(instance: &'a Instance, baseline: &InstalledCode) -> Self {
        let group = instance.code();
        let code = instance.module().code_index(baseline.func());
        let code = code.expect("a function with code");
        Self {
            instance,
            group,
            code,
            function: group.existing_function(code).expect("it was compiled"),
            interval: first_countdown(instance.engine()),
        }
    }

    fn entry_countdown(&self) -> &'a AtomicI32 {
        &self.group.slot(self.code).countdown
    }

    fn set_loop(&self, index: u32, value: i32) {
        self.function.loop_countdowns[index as usize].store(value, Ordering::Relaxed);
    }

    /// Until this holds, all loops count from `interval`.
    fn is_hot(&self, heat: &mut Heat) -> bool {
        if !heat.is_hot {
            let loops = self.function.loop_countdowns.iter();
            let total = loops
                .map(|countdown| counted(countdown, self.interval))
                .chain([heat.reported, heat.counted_at_entry(self.entry_countdown())])
                .fold(0, u32::saturating_add);
            heat.is_hot = total >= threshold(self.instance.engine());
        }
        heat.is_hot
    }

    fn at_entry(&self, state: &mut State) -> Option<Kind> {
        if !self.is_hot(&mut state.heat) {
            state.heat.set_entry(self.entry_countdown(), self.interval);
            return None;
        }
        self.start_optimizing(state)
    }

    fn start_optimizing(&self, state: &mut State) -> Option<Kind> {
        state.heat.set_entry(self.entry_countdown(), NEVER);
        matches!(state.optimized, Optimized::NotStarted).then(|| {
            state.optimized = Optimized::Started;
            Kind::Optimized
        })
    }

    /// There is no entry for the loop.
    fn in_loop(&self, state: &mut State, index: u32) -> Option<Kind> {
        let countdown = &self.function.loop_countdowns[index as usize];
        if !self.is_hot(&mut state.heat) {
            let turns = counted(countdown, self.interval);
            state.heat.reported = state.heat.reported.saturating_add(turns);
            self.set_loop(index, self.interval);
            return None;
        }
        self.set_loop(index, self.interval);
        let increment = entry_increment(self.instance.module(), self.code) as u32;
        let entries = state.heat.counted_at_entry(self.entry_countdown()) / increment;
        match state.optimized {
            Optimized::NotStarted if entries > FEW_ENTRIES => return self.start_optimizing(state),
            Optimized::NotStarted => state.heat.set_entry(self.entry_countdown(), AT_ONCE),
            Optimized::Started => return None,
            Optimized::Done(_) => {}
            Optimized::Failed => {
                self.set_loop(index, NEVER);
                return None;
            }
        }
        let loops = state.heat.loops.get_or_insert_with(|| {
            Box::new(Loops {
                nesting: Nesting::new(&self.instance.module().body(self.code)),
                candidate: None,
                is_compiling: false,
                attempted: SmallVec::new(),
                impatience: 0,
            })
        });
        let chosen = self.choose_loop(loops, index)?;
        loops.candidate = None;
        loops.impatience = 0;
        loops.is_compiling = true;
        loops.attempted.push(chosen);
        Some(Kind::LoopEntry(chosen))
    }

    fn choose_loop(&self, loops: &mut Loops, index: u32) -> Option<u32> {
        if loops.is_compiling {
            return None;
        }
        if loops.attempted.len() == MAX_LOOP_ENTRIES || loops.attempted.contains(&index) {
            self.set_loop(index, NEVER);
            return None;
        }
        let nesting = &loops.nesting;
        if nesting
            .around(index)
            .any(|outer| loops.attempted.contains(&outer))
        {
            loops.impatience += 1;
            return (loops.impatience == PATIENCE).then_some(index);
        }
        match loops.candidate {
            Some(candidate) if candidate == index || nesting.is_inside(index, candidate) => {
                return Some(candidate);
            }
            Some(candidate) if nesting.is_inside(candidate, index) => {}
            _ => nesting
                .around(index)
                .for_each(|outer| self.set_loop(outer, AT_ONCE)),
        }
        loops.candidate = Some(index);
        nesting.around(index).next().is_none().then_some(index)
    }

    fn start(&self, kind: Kind) {
        let (module, mode, code) = (self.instance.module(), self.instance.mode(), self.code);
        if !self.instance.engine().options().use_threads {
            let compilers = &mut self.instance.store().compilers.borrow_mut();
            return compile(module, mode, code, kind, compilers);
        }
        let priority = match kind {
            Kind::LoopEntry(_) => Priority::LoopEntry,
            Kind::Baseline | Kind::Optimized => Priority::Optimization,
        };
        let module = Arc::downgrade(module);
        queue::schedule(priority, move |workspace| {
            if let Some(module) = Weak::upgrade(&module) {
                compile(&module, mode, code, kind, &mut workspace.compilers);
            }
        });
    }
}

fn compile(
    module: &Module,
    mode: MemoryMode,
    code: CodeIndex,
    kind: Kind,
    compilers: &mut Compilers,
) {
    let group = module.code(mode);
    let function = group.existing_function(code).expect("it was compiled");
    let Kind::LoopEntry(index) = kind else {
        return match group.optimize(module, code, kind, compilers) {
            Some(optimized) => group.install_optimized(code, optimized),
            None => function.state.lock().give_up_optimizing(),
        };
    };
    let baseline = function.state.lock().lingering_baseline.upgrade();
    let Some(baseline) = baseline else { return };
    if let Some(entry) = group.optimize(module, code, kind, compilers) {
        baseline.add_loop_entry(index, entry);
        function.loop_countdowns[index as usize].store(AT_ONCE, Ordering::Relaxed);
    }
    let heat = &mut function.state.lock().heat;
    heat.loops.as_mut().expect("a loop was chosen").is_compiling = false;
}

/// # Safety
/// For the stub, which baseline code called from `pc`.
#[native_abi]
pub(crate) unsafe fn tier_up_at_entry(instance: &Instance, _fp: usize, pc: usize) {
    // SAFETY: the caller's.
    let baseline = unsafe { InstalledCode::running_at(pc - 1) }.expect("baseline code");
    let reporter = Reporter::new(instance, baseline);
    let state = &reporter.function.state;
    if !instance.store().has_stack_for(STACK_FOR_COMPILERS) {
        let countdown = reporter.entry_countdown();
        return state.lock().heat.set_entry(countdown, reporter.interval);
    }
    let wanted = reporter.at_entry(&mut state.lock());
    if let Some(kind) = wanted {
        reporter.start(kind);
    }
}

/// # Safety
/// For the stub, which baseline code with its frame at `fp` called from `pc`.
#[native_abi]
pub(crate) unsafe fn tier_up_in_loop(
    instance: &Instance,
    fp: usize,
    pc: usize,
    index: u32,
) -> usize {
    // SAFETY: the caller's.
    let baseline = unsafe { InstalledCode::running_at(pc - 1) }.expect("baseline code");
    let reporter = Reporter::new(instance, baseline);
    if !instance.store().has_stack_for(STACK_FOR_COMPILERS) {
        reporter.set_loop(index, reporter.interval);
        return 0;
    }
    let entry = baseline.loop_entry(index).or_else(|| {
        let wanted = reporter.in_loop(&mut reporter.function.state.lock(), index)?;
        reporter.start(wanted);
        baseline.loop_entry(index)
    });
    let Some((address, frame_size)) = entry else {
        return 0;
    };
    if fp.saturating_sub(frame_size) < instance.store().stack_limit() {
        reporter.set_loop(index, reporter.interval);
        return 0;
    }
    reporter.set_loop(index, AT_ONCE);
    address
}
