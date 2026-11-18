//! What the compiler hands the run time. Nothing here holds a `Gc` or a `Value`: cells are addresses, as in the graph.

use bun_engine_asm::{Code, Gpr, abi};
use bun_engine_bytecode::Register;

use crate::graph::{Assumption, ExitKind, Format, FrameIndex, Layout, Position};

/// The addresses that [`bun_engine_exec_memory::link`] is asked for.
pub(crate) mod symbol {
    /// The thunk that an exit jumps to, with the number of the exit in [`super::EXIT_REGISTER`].
    pub(crate) const EXIT: u32 = 0;
}

/// Where the code that leaves leaves the number of the exit. The back end never allocates it.
#[cfg(target_arch = "x86_64")]
pub(crate) const EXIT_REGISTER: Gpr = abi::SCRATCH_GPR;
/// Not the first of them: a jump that leaves the code may overwrite that one.
#[cfg(not(target_arch = "x86_64"))]
pub(crate) const EXIT_REGISTER: Gpr = abi::SCRATCH_GPRS[1];

/// Where a value is when the code stops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// At this offset from the frame pointer.
    Frame(i32),
    /// The value is the frame pointer plus this.
    FrameAddress(i32),
    Gpr(u8),
    Fpr(u8),
    /// The bits of a tagged value, or of whatever the format says.
    Constant(u64),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ExitValue {
    pub(crate) place: Place,
    pub(crate) format: Format,
}

/// Why and where the code stopped, and what it leaves behind.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ExitRecord {
    pub(crate) kind: ExitKind,
    /// Where the exit's own code begins.
    pub(crate) offset: u32,
    /// The instruction whose speculation failed.
    pub(crate) frame: FrameIndex,
    pub(crate) bytecode_offset: u32,
    /// A position in [`Output::layouts`].
    pub(crate) layout: u32,
    /// A range of [`Output::exit_values`], in the order in which the layout names the values.
    pub(crate) first_value: u32,
    pub(crate) value_count: u32,
}

/// A place at which the stack can be walked, for unwinding and for reading an inlined call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallSite {
    /// What the code writes into the frame before the call: the position of this record.
    pub(crate) site: u32,
    /// Where the callee returns to.
    pub(crate) return_offset: u32,
    pub(crate) frame: FrameIndex,
    pub(crate) bytecode_offset: u32,
    /// `NO_LAYOUT` where nothing of the frame is recorded.
    pub(crate) layout: u32,
    pub(crate) first_value: u32,
    pub(crate) value_count: u32,
}

/// Where an exception that a call site throws is caught.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Handler {
    pub(crate) site: u32,
    /// The frame that catches, which is the one that called or one of its callers.
    pub(crate) frame: FrameIndex,
    /// Where the handler starts in the bytecode of that frame.
    pub(crate) target: u32,
    /// Where to resume in the code, with the frame pointer, the stack pointer and the pinned registers restored.
    pub(crate) offset: u32,
}

/// A place that is written over with a jump once an assumption no longer holds. It is
/// [`bun_engine_asm::patch::REPLACEMENT_JUMP_SIZE`] bytes of nothing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct InvalidationPoint {
    pub(crate) offset: u32,
    /// Where the jump goes: the exit that leaves the code.
    pub(crate) target: u32,
}

pub(crate) struct Output {
    pub(crate) code: Code,
    /// Where a call enters, or where code that takes over a frame at a loop begins.
    pub(crate) entry: u32,
    /// From the stack pointer up to the frame pointer.
    pub(crate) frame_size: u32,
    /// Ascending by `site`.
    pub(crate) call_sites: Vec<CallSite>,
    pub(crate) call_site_values: Vec<ExitValue>,
    /// By the number that the exit's code loads.
    pub(crate) exits: Vec<ExitRecord>,
    pub(crate) exit_values: Vec<ExitValue>,
    /// Ascending by `site`, then by `target`.
    pub(crate) handlers: Vec<Handler>,
    pub(crate) invalidation_points: Vec<InvalidationPoint>,
    /// The cells the code names. It dies with any of them.
    pub(crate) weak_references: Vec<usize>,
    /// What to register before the code is installed. A `Stable` names a position in `Dependencies::shapes`.
    pub(crate) assumptions: Vec<ResolvedAssumption>,
    /// Moved out of the graph.
    pub(crate) frames: Vec<crate::graph::FrameInfo>,
    pub(crate) layouts: Vec<Layout>,
    pub(crate) layout_registers: Vec<Register>,
    /// What an `Origin` stands for, which is what `Layout::resume` needs.
    pub(crate) positions: Vec<Position>,
}

/// An [`Assumption`] with its shape resolved to an address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResolvedAssumption {
    /// No instance of the shape at this address changes to another.
    Stable(usize),
    /// The watchpoint of the realm whose global object is at this address.
    Realm(usize, bun_engine_vm::RealmWatchpoint),
}

impl ResolvedAssumption {
    pub(crate) fn of(assumption: Assumption, address_of_shape: impl Fn(u32) -> usize) -> Self {
        match assumption {
            Assumption::Stable(shape) => Self::Stable(address_of_shape(shape.0)),
            Assumption::Realm(global, which) => Self::Realm(global, which),
        }
    }
}
