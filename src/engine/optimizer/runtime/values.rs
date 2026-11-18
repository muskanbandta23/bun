//! Where the values of a state are, which frame each belongs to, and what they stand for.

use bun_engine_bytecode::Register;
use bun_engine_vm::Value;
use smallvec::SmallVec;

use super::machine::Saved;
use crate::graph::{Format, FrameIndex, NO_LAYOUT};
use crate::lower::output::{ExitValue, Output, Place};

/// What one frame of a state keeps, in the order in which the layout names it.
pub(crate) struct StateFrame<'a> {
    pub(crate) frame: FrameIndex,
    /// The instruction to run again or, for a caller, the one that is waiting for the call to return.
    pub(crate) resume: u32,
    pub(crate) registers: &'a [Register],
    pub(crate) values: &'a [ExitValue],
}

/// The frames of a state, the outermost first. Its values are those of each in turn.
pub(crate) fn frames<'a>(
    output: &'a Output,
    layout: u32,
    values: &'a [ExitValue],
) -> SmallVec<[StateFrame<'a>; 6]> {
    let mut chain: SmallVec<[u32; 6]> = SmallVec::new();
    let mut next = layout;
    while next != NO_LAYOUT {
        chain.push(next);
        next = output.layouts[next as usize].caller;
    }
    chain.reverse();
    let mut first_value = 0;
    let mut frames = SmallVec::new();
    for index in chain {
        let layout = output.layouts[index as usize];
        let registers = &output.layout_registers[layout.first_register as usize..]
            [..layout.register_count as usize];
        let position = output.positions[layout.resume.0 as usize];
        frames.push(StateFrame {
            frame: position.frame,
            resume: position.offset,
            registers,
            values: &values[first_value..][..registers.len()],
        });
        first_value += registers.len();
    }
    debug_assert_eq!(first_value, values.len());
    frames
}

/// What is in a place, in the form that the format names.
///
/// # Safety
/// A frame of the code that the state belongs to is at `frame_pointer`, and `saved` holds the registers of that code.
pub(crate) unsafe fn read(place: Place, frame_pointer: usize, saved: Option<&Saved>) -> u64 {
    let slot = |offset: i32| frame_pointer.wrapping_add_signed(offset as isize);
    match place {
        // SAFETY: the caller's: a slot of the frame.
        Place::Frame(offset) => unsafe { *core::ptr::with_exposed_provenance::<u64>(slot(offset)) },
        Place::FrameAddress(offset) => slot(offset) as u64,
        Place::Gpr(number) => saved.expect("registers are read at an exit").gpr(number),
        Place::Fpr(number) => saved.expect("registers are read at an exit").fpr(number),
        Place::Constant(bits) => bits,
    }
}

/// The bits of the value that the lower tier expects in the slot.
pub(crate) fn tagged(bits: u64, format: Format) -> u64 {
    match format {
        Format::Tagged => bits,
        Format::Int32 => Value::int32(bits as i32).to_bits(),
        Format::Int52 => Value::number(bits as i64 as f64).to_bits(),
        Format::Double => Value::double(f64::from_bits(bits)).to_bits(),
        Format::Boolean => Value::boolean(bits != 0).to_bits(),
        Format::Virtual => unreachable!("every object has been made"),
    }
}
