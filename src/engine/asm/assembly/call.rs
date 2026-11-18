use smallvec::SmallVec;

use super::FloatAssembly;
use crate::operand::Address;
use crate::register::{Fpr, Gpr, Reg, RegisterSet};

/// An argument of a C function. Addresses have no index.
#[derive(Clone, Copy, Debug)]
pub enum Argument {
    Gpr(Gpr),
    Fpr(Fpr),
    Imm(i64),
    Load32(Address),
    Load64(Address),
    LoadF64(Address),
    AddressOf(Address),
}

impl From<Gpr> for Argument {
    fn from(reg: Gpr) -> Self {
        Self::Gpr(reg)
    }
}

impl From<Fpr> for Argument {
    fn from(reg: Fpr) -> Self {
        Self::Fpr(reg)
    }
}

impl From<i32> for Argument {
    fn from(value: i32) -> Self {
        Self::Imm(value.into())
    }
}

impl From<u32> for Argument {
    fn from(value: u32) -> Self {
        Self::Imm(value.into())
    }
}

impl From<usize> for Argument {
    fn from(value: usize) -> Self {
        Self::Imm(value as i64)
    }
}

impl Argument {
    fn is_float(self) -> bool {
        matches!(self, Self::Fpr(_) | Self::LoadF64(_))
    }

    /// The register it reads in the bank of its destination.
    fn source(self) -> Option<Reg> {
        match self {
            Self::Gpr(reg) => Some(reg.into()),
            Self::Fpr(reg) => Some(reg.into()),
            Self::Imm(_) | Self::LoadF64(_) => None,
            Self::Load32(address) | Self::Load64(address) | Self::AddressOf(address) => {
                assert!(!address.has_index());
                Some(address.base.into())
            }
        }
    }

    fn with_source(self, reg: Reg) -> Self {
        let rebase = |address: Address| Address::new(reg.gpr(), address.offset);
        match self {
            Self::Gpr(_) => Self::Gpr(reg.gpr()),
            Self::Fpr(_) => Self::Fpr(reg.fpr()),
            Self::Load32(address) => Self::Load32(rebase(address)),
            Self::Load64(address) => Self::Load64(rebase(address)),
            Self::AddressOf(address) => Self::AddressOf(rebase(address)),
            Self::Imm(_) | Self::LoadF64(_) => self,
        }
    }
}

type Moves = SmallVec<[(Reg, Argument); 8]>;

pub trait CallAssembly: FloatAssembly {
    /// The bytes at the stack pointer that `move_arguments` writes.
    fn stack_size_of_arguments(arguments: &[Argument]) -> u32 {
        let floats = arguments
            .iter()
            .filter(|argument| argument.is_float())
            .count();
        let on_stack = floats.saturating_sub(Self::ARGUMENT_FPRS.len())
            + (arguments.len() - floats).saturating_sub(Self::ARGUMENT_GPRS.len());
        on_stack as u32 * 8
    }

    /// Puts the arguments where a C function finds them. Those that go on the stack are 64 bits wide.
    fn move_arguments(&mut self, arguments: &[Argument]) {
        let scratch = self.scratch();
        let temp = scratch.gprs[0].expect("arguments are moved through a scratch register");
        let (mut gprs, mut fprs) = (Self::ARGUMENT_GPRS.iter(), Self::ARGUMENT_FPRS.iter());
        let (mut gpr_moves, mut fpr_moves) = (Moves::new(), Moves::new());
        let mut slot = Address::new(Self::STACK_POINTER, 0);
        for &argument in arguments {
            if argument.is_float() {
                if let Some(&dst) = fprs.next() {
                    fpr_moves.push((dst.into(), argument));
                    continue;
                }
            } else if let Some(&dst) = gprs.next() {
                gpr_moves.push((dst.into(), argument));
                continue;
            }
            match argument {
                Argument::Gpr(src) => self.store64(slot, src),
                Argument::Fpr(src) => self.store_f64(slot, src),
                Argument::Imm(value) => self.store64(slot, value),
                Argument::Load64(src) | Argument::LoadF64(src) => self.transfer64(slot, src),
                Argument::Load32(_) | Argument::AddressOf(_) => {
                    move_one(self, temp.into(), argument);
                    self.store64(slot, temp);
                }
            }
            slot = slot.plus(8);
        }
        if !fpr_moves.is_empty() {
            let temp = scratch.fprs[0].expect("arguments are moved through a scratch register");
            move_in_parallel(self, fpr_moves, Argument::Fpr, Reg::fpr, temp.into());
        }
        move_in_parallel(self, gpr_moves, Argument::Gpr, Reg::gpr, temp.into());
    }

    /// Stores 8 bytes of each general register and then `vector_bytes`, 8 or 16, of each of the others.
    fn store_registers(&mut self, registers: RegisterSet, vector_bytes: i32, mut dst: Address) {
        let mut gprs = registers.gprs();
        while let Some(low) = gprs.next() {
            if let Some(high) = gprs.next() {
                self.store_pair64(dst, low, high);
                dst = dst.plus(16);
            } else {
                self.store64(dst, low);
                dst = dst.plus(8);
            }
        }
        let mut fprs = registers.fprs();
        while let Some(low) = fprs.next() {
            match (fprs.next(), vector_bytes) {
                (Some(high), 8) => self.store_pair_f64(dst, low, high),
                (Some(high), _) => self.store_pair_vector(dst, low, high),
                (None, 8) => self.store_f64(dst, low),
                (None, _) => self.store_vector(dst, low),
            }
            dst = dst.plus(2 * vector_bytes);
        }
    }

    /// Undoes `store_registers`. `src` uses none of the registers.
    fn load_registers(&mut self, registers: RegisterSet, vector_bytes: i32, mut src: Address) {
        let mut gprs = registers.gprs();
        while let Some(low) = gprs.next() {
            if let Some(high) = gprs.next() {
                self.load_pair64(low, high, src);
                src = src.plus(16);
            } else {
                self.load64(low, src);
                src = src.plus(8);
            }
        }
        let mut fprs = registers.fprs();
        while let Some(low) = fprs.next() {
            match (fprs.next(), vector_bytes) {
                (Some(high), 8) => self.load_pair_f64(low, high, src),
                (Some(high), _) => self.load_pair_vector(low, high, src),
                (None, 8) => self.load_f64(low, src),
                (None, _) => self.load_vector(low, src),
            }
            src = src.plus(2 * vector_bytes);
        }
    }
}

impl<M: FloatAssembly> CallAssembly for M {}

fn move_one(masm: &mut impl FloatAssembly, dst: Reg, argument: Argument) {
    match argument {
        Argument::Gpr(src) => masm.move64(dst.gpr(), src),
        Argument::Fpr(src) => masm.move_float(dst.fpr(), src),
        Argument::Imm(value) => masm.move64(dst.gpr(), value),
        Argument::Load32(src) => masm.load32(dst.gpr(), src),
        Argument::Load64(src) => masm.load64(dst.gpr(), src),
        Argument::LoadF64(src) => masm.load_f64(dst.fpr(), src),
        Argument::AddressOf(address) => masm.compute_address(dst.gpr(), address),
    }
}

/// Each move reads one register at most, so a cycle that is opened with `temp` unwinds before another needs it.
fn move_in_parallel<R>(
    masm: &mut impl FloatAssembly,
    mut moves: Moves,
    copy_of: impl Fn(R) -> Argument,
    typed: impl Fn(Reg) -> R,
    temp: Reg,
) {
    while let Some(&(blocked, _)) = moves.first() {
        let before = moves.len();
        let mut index = 0;
        while index < moves.len() {
            let (dst, argument) = moves[index];
            let is_read = moves
                .iter()
                .enumerate()
                .any(|(other, (_, reader))| other != index && reader.source() == Some(dst));
            if is_read {
                index += 1;
            } else {
                move_one(masm, dst, argument);
                moves.swap_remove(index);
            }
        }
        if moves.len() == before {
            move_one(masm, temp, copy_of(typed(blocked)));
            for (_, reader) in &mut moves {
                if reader.source() == Some(blocked) {
                    *reader = reader.with_source(temp);
                }
            }
        }
    }
}
