//! From bytes to a module. Functions are validated on several threads.

use core::mem;
use std::collections::VecDeque;
use std::sync::Arc;

use bun_engine_wasm_format::{
    Bodies, CodeIndex, Error, OutOfMemory, Parser, Scratch, UnvalidatedModule, ValidatedBodies,
};
use bun_threading::{Condition, Guarded};

use crate::engine::Engine;
use crate::module::Module;
use crate::queue::{self, Priority};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CompileError {
    Invalid(Error),
    OutOfMemory,
}

impl From<Error> for CompileError {
    fn from(error: Error) -> Self {
        Self::Invalid(error)
    }
}

impl From<OutOfMemory> for CompileError {
    fn from(_: OutOfMemory) -> Self {
        Self::OutOfMemory
    }
}

pub type Compiled = Result<Arc<Module>, CompileError>;

/// With less to do, waking another thread does not pay.
const BYTES_PER_HELPER: usize = 64 << 10;

#[derive(Default)]
struct Validation {
    state: Guarded<State>,
    is_done: Condition,
}

#[derive(Default)]
struct State {
    /// In order.
    waiting: VecDeque<Bodies>,
    waiting_bytes: usize,
    in_progress: usize,
    helpers: usize,
    outcomes: Vec<ValidatedBodies>,
    /// What follows does not matter.
    first_invalid: Option<CodeIndex>,
    when_done: Option<Box<dyn FnOnce(Vec<ValidatedBodies>) + Send>>,
}

impl Validation {
    /// `least_helpers` is for a caller that does not help.
    fn add(
        self: &Arc<Self>,
        engine: &Engine,
        bodies: impl Iterator<Item = Bodies>,
        least_helpers: usize,
    ) {
        let mut state = self.state.lock();
        for bodies in bodies {
            state.waiting_bytes += bodies.size_in_bytes();
            state.waiting.push_back(bodies);
        }
        if !engine.options().use_threads || state.waiting.is_empty() {
            return;
        }
        let wanted = (state.waiting_bytes / BYTES_PER_HELPER).max(least_helpers);
        let missing = wanted
            .min(queue::max_threads())
            .saturating_sub(state.helpers);
        state.helpers += missing;
        drop(state);
        for _ in 0..missing {
            let validation = Arc::clone(self);
            queue::schedule(Priority::Validation, move |workspace| {
                validation.help(&mut workspace.validation, 1);
            });
        }
    }

    /// Until nothing waits. `helpers` is 1 if the caller was counted.
    fn help(&self, scratch: &mut Scratch, helpers: usize) {
        let mut state = self.state.lock();
        while let Some(bodies) = state.waiting.pop_front() {
            state.waiting_bytes -= bodies.size_in_bytes();
            let first = bodies.first();
            if state.first_invalid.is_some_and(|invalid| invalid < first) {
                continue;
            }
            state.in_progress += 1;
            drop(state);
            let outcome = bodies.validate(scratch);
            state = self.state.lock();
            state.in_progress -= 1;
            if outcome.error().is_some() {
                let earliest = state.first_invalid.map_or(first, |other| other.min(first));
                state.first_invalid = Some(earliest);
            }
            state.outcomes.push(outcome);
        }
        state.helpers -= helpers;
        if state.in_progress != 0 {
            return;
        }
        self.is_done.broadcast();
        if let Some(when_done) = state.when_done.take() {
            let outcomes = mem::take(&mut state.outcomes);
            drop(state);
            when_done(outcomes);
        }
    }

    fn abandon(&self) {
        let mut state = self.state.lock();
        state.waiting.clear();
        state.waiting_bytes = 0;
    }
}

fn complete(
    engine: &'static Engine,
    module: UnvalidatedModule,
    outcomes: Vec<ValidatedBodies>,
) -> Compiled {
    Ok(Module::new(engine, module.complete(outcomes)?)?)
}

impl Engine {
    pub fn compile(&'static self, bytes: Vec<u8>) -> Compiled {
        let mut parser = Parser::new(self.options().features);
        parser.feed_owned(bytes)?;
        let mut module = parser.finish()?;
        let validation = Arc::<Validation>::default();
        validation.add(self, core::iter::from_fn(|| module.take_bodies()), 0);
        validation.help(&mut Scratch::default(), 0);
        let mut state = validation.state.lock();
        while state.in_progress != 0 {
            validation.is_done.wait_guarded(&mut state);
        }
        let outcomes = mem::take(&mut state.outcomes);
        drop(state);
        complete(self, module, outcomes)
    }

    /// `done` is called on any thread.
    pub fn compile_in_background(
        &'static self,
        bytes: Vec<u8>,
        done: impl FnOnce(Compiled) + Send + 'static,
    ) {
        self.run_in_background(move |_| {
            let mut compiler = StreamingCompiler::new(self);
            let _ = compiler.feed(bytes);
            compiler.finish(done);
        });
    }

    fn run_in_background(&self, job: impl FnOnce(&mut Scratch) + Send + 'static) {
        if self.options().use_threads {
            queue::schedule(Priority::Validation, |workspace| {
                job(&mut workspace.validation)
            });
        } else {
            job(&mut Scratch::default());
        }
    }
}

/// Validates functions while the rest of the module arrives.
pub struct StreamingCompiler {
    engine: &'static Engine,
    parser: Option<Parser>,
    validation: Arc<Validation>,
}

impl StreamingCompiler {
    pub fn new(engine: &'static Engine) -> Self {
        Self {
            engine,
            parser: Some(Parser::new(engine.options().features)),
            validation: Arc::default(),
        }
    }

    /// An error is final, and `finish` reports the same.
    pub fn feed(&mut self, bytes: Vec<u8>) -> Result<(), Error> {
        let parser = self.parser.as_mut().expect("not finished");
        parser
            .feed_owned(bytes)
            .inspect_err(|_| self.validation.abandon())?;
        let bodies = core::iter::from_fn(|| parser.take_bodies());
        self.validation.add(self.engine, bodies, 1);
        Ok(())
    }

    /// `done` is called on any thread, perhaps on this one before the return.
    pub fn finish(mut self, done: impl FnOnce(Compiled) + Send + 'static) {
        let (engine, validation) = (self.engine, Arc::clone(&self.validation));
        let mut module = match self.parser.take().expect("not finished").finish() {
            Ok(module) => module,
            Err(error) => {
                validation.abandon();
                return done(Err(error.into()));
            }
        };
        validation.add(engine, core::iter::from_fn(|| module.take_bodies()), 0);
        engine.run_in_background(move |scratch| {
            let when_done = move |outcomes| done(complete(engine, module, outcomes));
            validation.state.lock().when_done = Some(Box::new(when_done));
            validation.help(scratch, 0);
        });
    }
}

impl Drop for StreamingCompiler {
    fn drop(&mut self) {
        if self.parser.is_some() {
            self.validation.abandon();
        }
    }
}
