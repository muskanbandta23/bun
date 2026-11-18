//! A pattern is compiled for each width of subject, with and without captures, when that is first needed. It is
//! interpreted until machine code pays.

use core::cell::{Cell, UnsafeCell};
use core::sync::atomic::{AtomicUsize, Ordering};

use bun_engine_regexp::{Context, Error, Mode, Options, Outcome, Pattern, Program, Width, compile};
use bun_engine_text::Text;

use super::cell::Analysis;
use super::machine_code::{MachineCode, enter};
use crate::heap::Held;
use crate::{CaptureBuffer, Captures, Cx, JsResult, Match, RegExp, Thrown, Vm, options};

const WORK_BEFORE_TIER_UP: u64 = 2000;
const WORK_PER_SEARCH: u64 = 20;
const LONG_SUBJECT: usize = 1000;

const VARIANT_COUNT: usize = 4;

#[inline(always)]
fn variant(subject: Text<'_>, mode: Mode) -> usize {
    usize::from(!subject.is_latin1()) + usize::from(mode == Mode::Captures) * 2
}

/// The addresses of machine code for Latin-1 and for UTF-16, without captures and then with. Zero if there is none.
#[derive(Default)]
#[repr(transparent)]
pub(super) struct Entries([Cell<usize>; VARIANT_COUNT]);

#[derive(Default)]
pub(super) struct Code {
    programs: [Cell<Option<Box<Interpreted>>>; VARIANT_COUNT],
    /// What the `Entries` point into.
    machine_code: Cell<Vec<MachineCode>>,
}

struct Interpreted {
    program: Program,
    /// `None` if it stays interpreted.
    work_before_tier_up: Option<u64>,
}

/// What all searches of a `Vm` work with.
pub(super) struct Matcher {
    /// The address of the `RegExp`, for a profiler.
    running: AtomicUsize,
    context: UnsafeCell<Context>,
    stops_at_poll: Cell<bool>,
}

impl Default for Matcher {
    fn default() -> Matcher {
        Matcher {
            running: AtomicUsize::new(0),
            context: UnsafeCell::new(Context::new()),
            stops_at_poll: Cell::new(false),
        }
    }
}

impl Matcher {
    #[inline(always)]
    fn with_context<R>(&self, running: usize, body: impl FnOnce(&mut Context) -> R) -> R {
        debug_assert!(self.running.load(Ordering::Relaxed) == 0);
        self.running.store(running, Ordering::Relaxed);
        // SAFETY: the callers in this file run only the matcher, whose one way out is the poll below.
        let result = body(unsafe { &mut *self.context.get() });
        self.running.store(0, Ordering::Relaxed);
        result
    }

    pub(super) fn initialize(&self, vm: &Vm) {
        let vm = core::ptr::from_ref(vm);
        let should_stop = move || {
            // SAFETY: owns the context.
            let vm = unsafe { &*vm };
            vm.regexps().matcher.stops_at_poll.get() || vm.is_termination_requested()
        };
        self.with_context(0, |context| {
            context.set_poll(Box::new(should_stop));
            context.set_max_stack_size(options().max_regexp_stack_size);
        });
    }

    pub(super) fn trim(&self) {
        self.with_context(0, Context::trim);
    }

    pub(super) fn running(&self) -> usize {
        self.running.load(Ordering::Relaxed)
    }
}

#[inline(always)]
fn finish(cx: &Cx, outcome: Outcome) -> JsResult<Option<Match>> {
    match outcome {
        Outcome::Match { start, end } => Ok(Some(Match { start, end })),
        Outcome::NoMatch => Ok(None),
        Outcome::Interrupted | Outcome::OutOfMemory => Err(give_up(cx, outcome)),
    }
}

#[cold]
#[inline(never)]
fn give_up(cx: &Cx, outcome: Outcome) -> Thrown {
    cx.regexps().matcher.trim();
    match outcome {
        Outcome::Interrupted => cx.throw_termination(),
        _ => cx.throw_out_of_memory(),
    }
}

#[inline(always)]
fn captures(offsets: &mut [u32], found: Option<Match>) -> Option<Captures<'_>> {
    let Match { start, end } = found?;
    offsets[..2].copy_from_slice(&[start, end]);
    Some(Captures::new(offsets))
}

impl RegExp {
    /// The first match from `start` on or, if sticky, at `start`. Nothing is collected unless it throws.
    #[inline(always)]
    pub fn find(&self, cx: &Cx, subject: Text<'_>, start: u32) -> JsResult<Option<Match>> {
        match self.entries.0[variant(subject, Mode::MatchOnly)].get() {
            0 => self.find_slow(cx, subject, start),
            // SAFETY: the entry for that variant.
            entry => unsafe { self.run_machine_code(cx, entry, subject, start, &mut []) },
        }
    }

    /// The same, with the groups.
    #[inline(always)]
    pub fn exec<'a>(
        &self,
        cx: &Cx,
        subject: Text<'_>,
        start: u32,
        buffer: &'a mut CaptureBuffer,
    ) -> JsResult<Option<Captures<'a>>> {
        let entry = self.entries.0[variant(subject, Mode::Captures)].get();
        if entry == 0 {
            return self.exec_slow(cx, subject, start, buffer);
        }
        let offsets = cx.or_out_of_memory(buffer.offsets(self.group_count()))?;
        // SAFETY: the entry for that variant, with room for the groups.
        let found = unsafe { self.run_machine_code(cx, entry, subject, start, &mut offsets[2..]) }?;
        Ok(captures(offsets, found))
    }

    /// # Safety
    /// As for `enter`.
    #[inline(always)]
    unsafe fn run_machine_code(
        &self,
        cx: &Cx,
        entry: usize,
        subject: Text<'_>,
        start: u32,
        groups: &mut [u32],
    ) -> JsResult<Option<Match>> {
        let matcher = &cx.regexps().matcher;
        // SAFETY: the caller's.
        let outcome = matcher.with_context(self.address(), |context| unsafe {
            enter(entry, subject, start, groups, context)
        });
        finish(cx, outcome)
    }

    #[inline(never)]
    fn find_slow(&self, cx: &Cx, subject: Text<'_>, start: u32) -> JsResult<Option<Match>> {
        let analysis = self.analyze(cx, subject, Mode::MatchOnly)?;
        match &analysis.form {
            Some(form) => Ok(form.find(subject, start as usize).map(Match::from)),
            None => self.interpret(cx, &analysis.code, subject, start, &mut []),
        }
    }

    #[inline(never)]
    fn exec_slow<'a>(
        &self,
        cx: &Cx,
        subject: Text<'_>,
        start: u32,
        buffer: &'a mut CaptureBuffer,
    ) -> JsResult<Option<Captures<'a>>> {
        let analysis = self.analyze(cx, subject, Mode::Captures)?;
        let offsets = cx.or_out_of_memory(buffer.offsets(self.group_count()))?;
        let found = match &analysis.form {
            Some(form) => form.find(subject, start as usize).map(Match::from),
            None => self.interpret(cx, &analysis.code, subject, start, &mut offsets[2..])?,
        };
        Ok(captures(offsets, found))
    }

    /// For a search of that kind.
    #[inline(always)]
    pub(super) fn analyze(
        &self,
        cx: &Cx,
        subject: Text<'_>,
        mode: Mode,
    ) -> JsResult<Held<'_, Analysis>> {
        match self.analysis() {
            Some(analysis) => Ok(analysis),
            None => (self.analyze_slow(cx, subject, mode)).map_err(|error| Self::throw(cx, error)),
        }
    }

    #[cold]
    #[inline(never)]
    fn analyze_slow(
        &self,
        vm: &Vm,
        subject: Text<'_>,
        mode: Mode,
    ) -> Result<Held<'_, Analysis>, Error> {
        let pattern = self.parse()?;
        if let Some(form) = pattern.form()? {
            vm.regexps().cache.keep(self);
            return Ok(self.set_analysis(Some(form), pattern));
        }
        let mode = match pattern.group_count() {
            0 => Mode::MatchOnly,
            _ => mode,
        };
        let interpreted = self.compile(vm, &pattern, subject, mode)?;
        let analysis = self.set_analysis(None, pattern);
        analysis.code.programs[variant(subject, mode)].set(Some(interpreted));
        Ok(analysis)
    }

    #[cold]
    fn compile(
        &self,
        vm: &Vm,
        pattern: &Pattern,
        subject: Text<'_>,
        mode: Mode,
    ) -> Result<Box<Interpreted>, Error> {
        let compile_options = Options {
            width: match subject {
                Text::Latin1(_) => Width::Latin1,
                Text::Utf16(_) => Width::Utf16,
            },
            mode,
            sample: subject,
            stack_limit: vm.hard_stack_limit(),
        };
        let program = compile(pattern.tree(), &compile_options)?;
        if self.code_size.load(Ordering::Relaxed) == 0 {
            vm.regexps().cache.keep(self);
        }
        self.account_for_code(vm, program.size(), 0);
        let work = match options().use_eager_regexp_jit {
            true => 0,
            false => WORK_BEFORE_TIER_UP,
        };
        Ok(Box::new(Interpreted {
            program,
            work_before_tier_up: MachineCode::is_enabled().then_some(work),
        }))
    }

    fn account_for_code(&self, vm: &Vm, added: usize, removed: usize) {
        let old = self.code_size.load(Ordering::Relaxed) as usize;
        let new = (old + added).saturating_sub(removed);
        (self.code_size).store(new.try_into().unwrap_or(u32::MAX), Ordering::Relaxed);
        vm.heap.note_extra_memory_allocated(self, added);
    }

    /// `groups` is empty, or has room for all.
    fn interpret(
        &self,
        cx: &Cx,
        code: &Code,
        subject: Text<'_>,
        start: u32,
        groups: &mut [u32],
    ) -> JsResult<Option<Match>> {
        let mode = match groups {
            [] => Mode::MatchOnly,
            _ => Mode::Captures,
        };
        let slot = &code.programs[variant(subject, mode)];
        let mut interpreted = match slot.take() {
            Some(interpreted) => interpreted,
            None => (self.parse())
                .and_then(|pattern| self.compile(cx, &pattern, subject, mode))
                .map_err(|error| Self::throw(cx, error))?,
        };
        let matcher = &cx.regexps().matcher;
        let mut is_long = subject.len().saturating_sub(start as usize) > LONG_SUBJECT;
        loop {
            if let Some(work) = interpreted.work_before_tier_up
                && (work == 0 || is_long)
            {
                if let Some(entry) = self.tier_up(cx, code, &interpreted.program, subject, mode) {
                    // SAFETY: just made for that variant.
                    return unsafe { self.run_machine_code(cx, entry, subject, start, groups) };
                }
                interpreted.work_before_tier_up = None;
            }
            (matcher.stops_at_poll).set(interpreted.work_before_tier_up.is_some());
            let (outcome, work) = matcher.with_context(self.address(), |context| {
                let outcome = interpreted.program.exec(subject, start, groups, context);
                (outcome, context.take_work())
            });
            if matcher.stops_at_poll.take()
                && outcome == Outcome::Interrupted
                && !cx.is_termination_requested()
            {
                is_long = true;
                continue;
            }
            if let Some(left) = &mut interpreted.work_before_tier_up {
                *left = left.saturating_sub(work + WORK_PER_SEARCH);
            }
            slot.set(Some(interpreted));
            return finish(cx, outcome);
        }
    }

    /// The caller drops the program, unless this fails.
    #[cold]
    fn tier_up(
        &self,
        vm: &Vm,
        code: &Code,
        program: &Program,
        subject: Text<'_>,
        mode: Mode,
    ) -> Option<usize> {
        let machine_code = MachineCode::new(vm, program)?;
        machine_code.describe(|| self.to_literal_text().to_string());
        self.account_for_code(vm, machine_code.size(), program.size());
        let entry = machine_code.entry();
        let mut all = code.machine_code.take();
        all.reserve_exact(1);
        all.push(machine_code);
        code.machine_code.set(all);
        self.entries.0[variant(subject, mode)].set(entry);
        if self.group_count() == 0 {
            self.entries.0[variant(subject, Mode::Captures)].set(entry);
        }
        Some(entry)
    }

    pub(super) fn delete_code(&self) {
        self.entries.0.iter().for_each(|entry| entry.set(0));
        if let Some(analysis) = self.analysis() {
            analysis.code.programs.iter().for_each(|it| it.set(None));
            analysis.code.machine_code.take();
        }
        self.code_size.store(0, Ordering::Relaxed);
    }
}
