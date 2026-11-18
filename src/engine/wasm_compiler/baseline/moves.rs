//! Moves that take place as if at once.

use bun_engine_asm::{Reg, RegisterSet};
use bun_engine_wasm_format::ValKind;

use super::Function;
use super::operand::{Operand, scratch};

/// Offsets are in the frame. The destination comes first.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Step {
    Copy(ValKind, Reg, Reg),
    Load(ValKind, Reg, i32),
    Constant(ValKind, Reg, u64),
    Store(ValKind, i32, Reg),
    StoreConstant(ValKind, i32, u64),
    Transfer(ValKind, i32, i32),
}

impl Step {
    /// `value` was located.
    pub(super) fn to_frame(dst: i32, value: Operand) -> Option<Self> {
        let kind = value.ty().kind();
        match value {
            Operand::Constant(_, bits) => Some(Self::StoreConstant(kind, dst, bits)),
            Operand::Register(_, reg) => Some(Self::Store(kind, dst, reg)),
            Operand::Slot(_, src) => (src != dst).then_some(Self::Transfer(kind, dst, src)),
            Operand::Local(..) => unreachable!(),
        }
    }
}

/// To registers, each of which is written once.
pub(super) struct Moves {
    copied_to: RegisterSet,
    /// By the destination.
    sources: [(ValKind, Reg); Reg::COUNT],
    /// What reads no register.
    last: Vec<Step>,
}

impl Default for Moves {
    fn default() -> Self {
        Self {
            copied_to: RegisterSet::EMPTY,
            sources: [(ValKind::I32, Reg::from_index(0)); Reg::COUNT],
            last: Vec::new(),
        }
    }
}

impl Moves {
    /// `value` was located.
    #[inline]
    pub(super) fn add(&mut self, dst: Reg, value: Operand) {
        let kind = value.ty().kind();
        match value {
            Operand::Register(_, src) if src == dst => {}
            Operand::Register(_, src) => {
                debug_assert!(!self.copied_to.contains(dst));
                self.copied_to.insert(dst);
                self.sources[dst.index()] = (kind, src);
            }
            Operand::Constant(_, bits) => self.last.push(Step::Constant(kind, dst, bits)),
            Operand::Slot(_, src) => self.last.push(Step::Load(kind, dst, src)),
            Operand::Local(..) => unreachable!(),
        }
    }

    /// Puts them in order, and forgets them.
    #[inline]
    pub(super) fn finish(&mut self, steps: &mut Vec<Step>) {
        if !self.copied_to.is_empty() {
            self.order_copies(steps);
        }
        steps.append(&mut self.last);
    }

    fn order_copies(&mut self, steps: &mut Vec<Step>) {
        let mut left = core::mem::take(&mut self.copied_to);
        while let Some(first) = left.first() {
            let read: RegisterSet = (left.into_iter())
                .map(|dst| self.sources[dst.index()].1)
                .collect();
            let ready = left - read;
            for dst in ready {
                let (kind, src) = self.sources[dst.index()];
                steps.push(Step::Copy(kind, dst, src));
            }
            left -= ready;
            if ready.is_empty() {
                // Only cycles are left. The one that is opened unwinds before the next needs the scratch register.
                let (kind, src) = &mut self.sources[first.index()];
                let saved = scratch(src.bank());
                steps.push(Step::Copy(*kind, saved, core::mem::replace(src, saved)));
            }
        }
    }
}

impl Function<'_> {
    #[inline]
    pub(super) fn add_move(&mut self, dst: impl Into<Reg>, value: Operand) {
        let value = self.locate(value);
        self.moves.add(dst.into(), value);
    }

    pub(super) fn emit_moves(&mut self) {
        let start = self.steps.len();
        self.moves.finish(&mut self.steps);
        self.emit_steps_from(start);
    }

    /// Removes them.
    #[inline]
    pub(super) fn emit_steps_from(&mut self, start: usize) {
        for index in start..self.steps.len() {
            self.emit_step(self.steps[index]);
        }
        self.steps.truncate(start);
    }

    pub(super) fn emit_step(&mut self, step: Step) {
        match step {
            Step::Copy(kind, dst, src) => self.copy(kind, dst, src),
            Step::Load(kind, dst, src) => self.load(kind, dst, self.frame_address(src)),
            Step::Constant(kind, dst, bits) => self.move_constant(kind, dst, bits),
            Step::Store(kind, dst, src) => self.store(kind, self.frame_address(dst), src),
            Step::StoreConstant(kind, dst, bits) => {
                self.store_constant(kind, self.frame_address(dst), bits);
            }
            Step::Transfer(kind, dst, src) => {
                self.transfer(kind, self.frame_address(dst), self.frame_address(src));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bun_engine_wasm_format::ValType;

    use super::*;

    const REGISTERS: usize = 5;

    /// Every way for a few registers of each bank to be written or not, from any of them.
    #[test]
    fn every_assignment_takes_place_at_once() {
        let mut moves = Moves::default();
        let mut steps = Vec::new();
        let choices = REGISTERS + 2;
        for (ty, first) in [(ValType::I64, 0), (ValType::V128, Reg::COUNT / 2)] {
            for mut number in 0..choices.pow(REGISTERS as u32) {
                let mut held: [u64; Reg::COUNT] = core::array::from_fn(|index| index as u64);
                let mut expected = held;
                for dst in first..first + REGISTERS {
                    let choice = number % choices;
                    number /= choices;
                    let value = match choice {
                        0..REGISTERS => Operand::Register(ty, Reg::from_index(first + choice)),
                        REGISTERS => Operand::Slot(ty, 1000),
                        _ => continue,
                    };
                    expected[dst] = match value {
                        Operand::Register(_, src) => src.index() as u64,
                        _ => 1000,
                    };
                    moves.add(Reg::from_index(dst), value);
                }
                moves.finish(&mut steps);
                for step in steps.drain(..) {
                    match step {
                        Step::Copy(_, dst, src) => held[dst.index()] = held[src.index()],
                        Step::Load(_, dst, src) => held[dst.index()] = src as u64,
                        _ => unreachable!(),
                    }
                }
                let range = first..first + REGISTERS;
                assert_eq!(held[range.clone()], expected[range]);
            }
        }
    }
}
