//! From a function to machine code.

mod allocate;
mod check;
mod code;
mod edges;
mod emit;
mod liveness;
mod lower;
mod moves;
mod shared;
mod slots;

pub(crate) use code::{Class, Place, Registers, Role};
pub use emit::Emitted;
pub(crate) use emit::{Emitter, Isa, Operands};
pub(crate) use lower::{Instruction, Lowering};
pub(crate) use shared::{Shared, SharesInstructions};

use bun_engine_asm::MacroAssembly;

use crate::TooComplex;
use crate::analysis::{Cfg, Frequencies};
use crate::client::{Client, Frame};
use crate::ir::{Function, Table};
use crate::passes::Level;

const MAX_FRAME_SIZE: u32 = 1 << 30;

pub(crate) struct Machine<I> {
    instructions: Vec<I>,
    code: code::Code,
    lowering: lower::LoweringTables,
    liveness: liveness::Liveness,
    allocation: allocate::Allocation,
    emission: emit::EmissionTables,
}

impl<I> Default for Machine<I> {
    fn default() -> Self {
        Self {
            instructions: Vec::new(),
            code: Default::default(),
            lowering: Default::default(),
            liveness: Default::default(),
            allocation: Default::default(),
            emission: Default::default(),
        }
    }
}

impl<I: Isa> Machine<I> {
    pub(crate) fn generate(
        &mut self,
        function: &mut Function,
        cfg: &Cfg,
        frequencies: &Frequencies,
        level: Level,
        records_origins: bool,
        masm: &mut I::Masm,
        client: &mut impl Client<I::Masm>,
    ) -> Result<(Frame, Emitted), TooComplex> {
        let mut taken = function.reserved_frame_bytes.next_multiple_of(8);
        let mut slot_offsets = Table::default();
        for slot in function.slots().values() {
            taken = taken
                .saturating_add(slot.size)
                .min(MAX_FRAME_SIZE)
                .next_multiple_of(slot.alignment.max(8));
            slot_offsets.push(-(taken as i32));
        }
        let features = masm.features();
        let Self {
            instructions,
            code,
            lowering,
            liveness,
            allocation,
            emission,
        } = self;
        lower::lower(
            function,
            cfg,
            frequencies,
            features,
            &slot_offsets,
            code,
            instructions,
            lowering,
        );
        liveness.compute(function, cfg, frequencies, code, level == Level::Full)?;
        allocate::allocate(
            function,
            cfg,
            frequencies,
            liveness,
            I::registers(function),
            code,
            allocation,
        );
        taken = allocation.slots.place(function, cfg, code, taken);
        if allocation.sequencer.used_spare {
            taken = (taken + 16).next_multiple_of(16);
        }
        let spare_offset = -(taken as i32);
        let size = taken.saturating_add(code.outgoing_bytes);
        if size >= MAX_FRAME_SIZE {
            return Err(TooComplex);
        }
        let frame = Frame {
            size: size.next_multiple_of(16),
            slot_offsets,
        };
        let places = emit::Places {
            function,
            code,
            allocation,
            spare_offset,
        };
        if cfg!(debug_assertions) {
            check::check(places, cfg, instructions);
        }
        let emitted = emit::emit(
            places,
            cfg,
            frequencies,
            instructions,
            &frame,
            emission,
            masm,
            client,
            records_origins,
        );
        Ok((frame, emitted))
    }
}
