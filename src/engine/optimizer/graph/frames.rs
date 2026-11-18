//! Where code comes from, and how to get back there.

use bun_engine_bytecode::{Register, Specialization};
use bun_engine_codegen::ir::Origin;

/// A position in `Graph::frames`. The first is the function that has the frame on the stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct FrameIndex(pub(crate) u32);

impl FrameIndex {
    pub(crate) const ROOT: Self = Self(0);

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

/// How the caller of an inlined function goes on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CallKind {
    Call,
    Construct,
    /// The frame of the caller is gone, as far as scripts can tell.
    TailCall,
    Getter,
    Setter,
}

impl CallKind {
    pub(crate) fn specialization(self) -> Specialization {
        if self == Self::Construct {
            Specialization::Construct
        } else {
            Specialization::Call
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FrameInfo {
    /// The address of the code block that has the profiles.
    pub(crate) code_block: usize,
    pub(crate) global: usize,
    /// Not of the root.
    pub(crate) caller: Origin,
    pub(crate) kind: CallKind,
    /// The words that the interpreter needs below the frame pointer.
    pub(crate) frame_words: u32,
    pub(crate) parameters: u32,
    pub(crate) depth: u32,
}

/// What an `Origin` stands for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Position {
    pub(crate) frame: FrameIndex,
    pub(crate) offset: u32,
}

/// What the values of a state are. Those of the callers come first.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    /// The instruction to run again or, for a caller, the one that is waiting for the call to return.
    pub(crate) resume: Origin,
    /// `NO_LAYOUT` for the root.
    pub(crate) caller: u32,
    /// A range of `Graph::layout_registers`, with one register for each value.
    pub(crate) first_register: u32,
    pub(crate) register_count: u32,
    /// A position in `Graph::recipes` for the values that follow those of all registers, or `NO_RECIPE`.
    pub(crate) recipe: u32,
}

pub(crate) const NO_LAYOUT: u32 = u32::MAX;
pub(crate) const NO_RECIPE: u32 = u32::MAX;

/// In a layout of an inlined function, where the number of arguments including `this` goes.
pub(crate) const ARGUMENT_COUNT: Register = Register::from_offset(4);

/// The tag of a value in a state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Format {
    Tagged,
    Int32,
    Int52,
    Double,
    Boolean,
    /// An object that has yet to be made. The value is an I32 constant, its position in the recipe.
    Virtual,
}

impl Format {
    pub(crate) fn from_tag(tag: u8) -> Self {
        [
            Self::Tagged,
            Self::Int32,
            Self::Int52,
            Self::Double,
            Self::Boolean,
            Self::Virtual,
        ][usize::from(tag)]
    }
}

pub(crate) use bun_engine_vm::ExitKind;

/// What the token of an exit stands for.
#[derive(Clone, Copy)]
pub(crate) struct ExitInfo {
    pub(crate) kind: ExitKind,
    /// The instruction whose speculation failed.
    pub(crate) origin: Origin,
}
