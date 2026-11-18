//! What functions of the run time return to generated code.

use bun_engine_wasm_compiler::abi::Trap;

use crate::host::Thrown;

/// In a word that is not zero.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub(crate) struct Failure(usize);

impl Failure {
    pub(crate) const THROWN: Self = Self(Trap::COUNT + 1);

    pub(crate) const fn trap(trap: Trap) -> Self {
        Self(trap as usize + 1)
    }

    pub(crate) const fn word(self) -> usize {
        self.0
    }

    /// `None` if the exception is pending already.
    pub(crate) fn as_trap(self) -> Option<Trap> {
        Trap::ALL.get(self.0 - 1).copied()
    }
}

impl From<Trap> for Failure {
    #[inline]
    fn from(trap: Trap) -> Self {
        Self::trap(trap)
    }
}

impl From<Thrown> for Failure {
    #[inline]
    fn from(_: Thrown) -> Self {
        Self::THROWN
    }
}

/// Comes back in two registers.
#[repr(C)]
pub(crate) struct Outcome {
    value: u64,
    /// Zero, or a [`Failure`].
    failure: usize,
}

impl Outcome {
    #[inline]
    pub(crate) fn value(value: u64) -> Self {
        Self { value, failure: 0 }
    }

    #[inline]
    pub(crate) fn failure(failure: impl Into<Failure>) -> Self {
        Self {
            value: 0,
            failure: failure.into().word(),
        }
    }
}

impl<E: Into<Failure>> From<Result<u64, E>> for Outcome {
    #[inline]
    fn from(result: Result<u64, E>) -> Self {
        result.map_or_else(Self::failure, Self::value)
    }
}

impl<E: Into<Failure>> From<Result<(), E>> for Outcome {
    #[inline]
    fn from(result: Result<(), E>) -> Self {
        result.map(|()| 0).into()
    }
}
