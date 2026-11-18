//! What a code block remembers of the attempts to optimize it.

use core::any::Any;
use core::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use bun_engine_bytecode::{Liveness, TooLarge};

use crate::{CodeBlock, GcFree};

/// Why optimized code was left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ExitKind {
    BadType,
    BadShape,
    BadElements,
    BadIdentity,
    BadName,
    BadExecutable,
    Overflow,
    Int52Overflow,
    NegativeZero,
    NotAnInteger,
    OutOfBounds,
    NegativeIndex,
    LoadFromHole,
    StoreToHole,
    TooManyArguments,
    ExoticObject,
    ArgumentsEscaped,
    /// A check that was moved out of a loop.
    HoistingFailed,
    /// The code had not run when it was compiled.
    NeverExecuted,
    /// The interpreter is going to throw.
    WillThrow,
    /// From here on they say nothing against the speculation.
    Invalidated,
    Exception,
    TierUp,
}

impl ExitKind {
    pub const fn from_u8(value: u8) -> ExitKind {
        use ExitKind::*;
        [
            BadType,
            BadShape,
            BadElements,
            BadIdentity,
            BadName,
            BadExecutable,
            Overflow,
            Int52Overflow,
            NegativeZero,
            NotAnInteger,
            OutOfBounds,
            NegativeIndex,
            LoadFromHole,
            StoreToHole,
            TooManyArguments,
            ExoticObject,
            ArgumentsEscaped,
            HoistingFailed,
            NeverExecuted,
            WillThrow,
            Invalidated,
            Exception,
            TierUp,
        ][value as usize]
    }

    pub const fn is_counted(self) -> bool {
        (self as u8) < ExitKind::Invalidated as u8
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ExitKinds(u32);

impl ExitKinds {
    pub fn contains(self, kind: ExitKind) -> bool {
        self.0 >> kind as u32 & 1 != 0
    }

    pub fn insert(&mut self, kind: ExitKind) {
        self.0 |= 1 << kind as u32;
    }
}

/// What the optimizer keeps for a code block that is not optimized.
pub trait OptimizerState: GcFree + Any {}

#[derive(Default)]
pub(super) struct Optimization {
    liveness: OnceCell<Rc<Liveness>>,
    /// Ascending by offset.
    exit_sites: RefCell<Vec<(u32, ExitKinds)>>,
    exits_anywhere: Cell<ExitKinds>,
    reoptimizations: Cell<u32>,
    state: OnceCell<Box<dyn OptimizerState>>,
}

impl CodeBlock {
    pub fn liveness(&self) -> Result<Rc<Liveness>, TooLarge> {
        let baseline = self.baseline();
        if let Some(liveness) = baseline.optimization.liveness.get() {
            return Ok(Rc::clone(liveness));
        }
        let liveness = Rc::new(Liveness::new(self.body())?);
        Ok(Rc::clone(
            baseline.optimization.liveness.get_or_init(|| liveness),
        ))
    }

    /// A speculation has failed often at this instruction, so it is not made again.
    pub fn add_exit_site(&self, bytecode_offset: u32, kind: ExitKind) {
        let baseline = self.baseline();
        let mut sites = baseline.optimization.exit_sites.borrow_mut();
        let position = sites
            .binary_search_by_key(&bytecode_offset, |&(offset, _)| offset)
            .unwrap_or_else(|position| {
                sites.insert(position, (bytecode_offset, ExitKinds::default()));
                position
            });
        sites[position].1.insert(kind);
        let mut anywhere = baseline.optimization.exits_anywhere.get();
        anywhere.insert(kind);
        baseline.optimization.exits_anywhere.set(anywhere);
    }

    pub fn exits_at(&self, bytecode_offset: u32) -> ExitKinds {
        let baseline = self.baseline();
        let sites = baseline.optimization.exit_sites.borrow();
        let found = sites.binary_search_by_key(&bytecode_offset, |&(offset, _)| offset);
        found.map_or_else(|_| ExitKinds::default(), |position| sites[position].1)
    }

    pub fn exits_anywhere(&self) -> ExitKinds {
        self.baseline().optimization.exits_anywhere.get()
    }

    /// How often optimized code has been thrown away.
    pub fn reoptimizations(&self) -> u32 {
        self.baseline().optimization.reoptimizations.get()
    }

    pub fn count_reoptimization(&self) {
        let baseline = self.baseline();
        baseline
            .optimization
            .reoptimizations
            .update(|count| count.saturating_add(1));
    }

    /// Of a code block that is not optimized.
    pub fn optimizer_state<T: OptimizerState + Default>(&self) -> &T {
        assert!(!self.is_optimized());
        let state: &dyn Any = &**self
            .optimization
            .state
            .get_or_init(|| Box::new(T::default()));
        state.downcast_ref().expect("there is one optimizer")
    }
}
