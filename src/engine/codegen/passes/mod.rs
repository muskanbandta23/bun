//! Transformations of functions. Each leaves a valid function without aliases.

mod eliminate_dead_code;
mod lower_total_division;

use crate::analysis::{Cfg, Dominators, Frequencies};
use crate::ir::{Function, validate};
use crate::target::Target;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// Only what instruction selection requires.
    Quick,
    Full,
}

/// Analyses, computed when a pass asks for them and kept until the control flow changes.
#[derive(Default)]
pub struct Analyses {
    cfg: Cfg,
    dominators: Dominators,
    frequencies: Frequencies,
    has_cfg: bool,
    has_dominators: bool,
    has_frequencies: bool,
}

impl Analyses {
    /// After blocks or edges have been added, removed or redirected.
    pub fn invalidate(&mut self) {
        (self.has_cfg, self.has_dominators, self.has_frequencies) = (false, false, false);
    }

    /// Also removes the blocks that cannot be reached.
    pub fn cfg(&mut self, function: &mut Function) -> &Cfg {
        if !self.has_cfg {
            self.cfg.compute(function);
            self.cfg.remove_unreachable_blocks(function);
            self.has_cfg = true;
        }
        &self.cfg
    }

    pub fn dominators(&mut self, function: &mut Function) -> (&Cfg, &Dominators) {
        self.cfg(function);
        if !self.has_dominators {
            self.dominators.compute(function, &self.cfg);
            self.has_dominators = true;
        }
        (&self.cfg, &self.dominators)
    }

    pub fn frequencies(&mut self, function: &mut Function) -> (&Cfg, &Frequencies) {
        self.cfg(function);
        if !self.has_frequencies {
            self.frequencies.compute(function, &self.cfg);
            self.has_frequencies = true;
        }
        (&self.cfg, &self.frequencies)
    }
}

pub struct Pass {
    pub name: &'static str,
    run: fn(&mut Function, &mut Analyses, Target),
}

macro_rules! passes {
    ($($name:ident),* $(,)?) => {
        &[$(Pass { name: stringify!($name), run: $name::run }),*]
    };
}

const QUICK: &[Pass] = passes![lower_total_division];
const FULL: &[Pass] = passes![eliminate_dead_code, lower_total_division];

impl Pass {
    pub fn pipeline(level: Level) -> &'static [Pass] {
        match level {
            Level::Quick => QUICK,
            Level::Full => FULL,
        }
    }

    pub fn run(&self, function: &mut Function, analyses: &mut Analyses, target: Target) {
        (self.run)(function, analyses, target);
        if cfg!(debug_assertions)
            && let Err(problem) = validate(function)
        {
            panic!("after {}: {problem}\n{function:?}", self.name);
        }
    }
}
