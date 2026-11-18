//! Values on the operand stack, and how each kind of value is moved.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Reg, RegOrImm};
use bun_engine_wasm_format::{ValKind, ValType};

use super::Function;

/// The representation puts the type at the same place in all variants.
#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Operand {
    /// One of 32 bits has the upper half clear. It is no vector.
    Constant(ValType, u64),
    /// Nothing else is in the register.
    Register(ValType, Reg),
    /// What the local holds now.
    Local(ValType, u32),
    /// At this offset in the frame.
    Slot(ValType, i32),
}

impl Operand {
    #[inline(always)]
    pub(super) fn ty(self) -> ValType {
        match self {
            Self::Constant(ty, _)
            | Self::Register(ty, _)
            | Self::Local(ty, _)
            | Self::Slot(ty, _) => ty,
        }
    }

    /// `ty` is of the same kind.
    pub(super) fn with_type(self, ty: ValType) -> Self {
        match self {
            Self::Constant(_, bits) => Self::Constant(ty, bits),
            Self::Register(_, reg) => Self::Register(ty, reg),
            Self::Local(_, local) => Self::Local(ty, local),
            Self::Slot(_, offset) => Self::Slot(ty, offset),
        }
    }

    #[inline(always)]
    pub(super) fn int(width: Width, value: u64) -> Self {
        match width {
            Width::W32 => Self::Constant(ValType::I32, u64::from(value as u32)),
            Width::W64 => Self::Constant(ValType::I64, value),
        }
    }

    #[inline(always)]
    pub(super) fn constant(self) -> Option<u64> {
        match self {
            Self::Constant(_, bits) => Some(bits),
            _ => None,
        }
    }
}

#[inline(always)]
pub(super) fn bank(kind: ValKind) -> Bank {
    match kind {
        ValKind::F32 | ValKind::F64 | ValKind::V128 => Bank::Fpr,
        ValKind::I32 | ValKind::I64 | ValKind::Ref => Bank::Gpr,
    }
}

#[inline(always)]
pub(super) fn untrusted(bits: u64) -> RegOrImm {
    Untrusted(bits).into()
}

#[cfg(target_arch = "x86_64")]
const SCRATCH_GPR: Gpr = abi::SCRATCH_GPR;
#[cfg(target_arch = "aarch64")]
const SCRATCH_GPR: Gpr = abi::SCRATCH_GPRS[0];
const SCRATCH_FPR: Fpr = abi::SCRATCH_FPRS[0];

/// The one that moves, loads and stores leave alone.
pub(super) fn scratch(bank: Bank) -> Reg {
    match bank {
        Bank::Gpr => SCRATCH_GPR.into(),
        Bank::Fpr => SCRATCH_FPR.into(),
    }
}

impl Function<'_> {
    #[cfg(target_arch = "x86_64")]
    #[inline(always)]
    pub(super) fn frame_address(&self, offset: i32) -> Address {
        Address::new(abi::FRAME_POINTER, offset)
    }

    /// Positive offsets reach further.
    #[cfg(target_arch = "aarch64")]
    #[inline(always)]
    pub(super) fn frame_address(&self, offset: i32) -> Address {
        if offset >= -256 {
            Address::new(abi::FRAME_POINTER, offset)
        } else {
            Address::new(abi::STACK_POINTER, self.stack_depth as i32 + offset)
        }
    }

    pub(super) fn load(&mut self, kind: ValKind, dst: Reg, src: Address) {
        match kind {
            ValKind::I32 => self.masm.load32(dst.gpr(), src),
            ValKind::I64 | ValKind::Ref => self.masm.load64(dst.gpr(), src),
            ValKind::F32 => self.masm.load_f32(dst.fpr(), src),
            ValKind::F64 => self.masm.load_f64(dst.fpr(), src),
            ValKind::V128 => self.masm.load_vector(dst.fpr(), src),
        }
    }

    pub(super) fn store(&mut self, kind: ValKind, dst: Address, src: Reg) {
        match kind {
            ValKind::I32 => self.masm.store32(dst, src.gpr()),
            ValKind::I64 | ValKind::Ref => self.masm.store64(dst, src.gpr()),
            ValKind::F32 => self.masm.store_f32(dst, src.fpr()),
            ValKind::F64 => self.masm.store_f64(dst, src.fpr()),
            ValKind::V128 => self.masm.store_vector(dst, src.fpr()),
        }
    }

    pub(super) fn copy(&mut self, kind: ValKind, dst: Reg, src: Reg) {
        match kind {
            ValKind::I32 => self.masm.move32(dst.gpr(), src.gpr()),
            ValKind::I64 | ValKind::Ref => self.masm.move64(dst.gpr(), src.gpr()),
            ValKind::F32 | ValKind::F64 => self.masm.move_float(dst.fpr(), src.fpr()),
            ValKind::V128 => self.masm.move_vector(dst.fpr(), src.fpr()),
        }
    }

    pub(super) fn move_constant(&mut self, kind: ValKind, dst: Reg, bits: u64) {
        match kind {
            ValKind::I32 | ValKind::I64 | ValKind::Ref => {
                self.masm.move64(dst.gpr(), Untrusted(bits));
            }
            ValKind::F32 => (self.masm).move_float_constant(FloatWidth::F32, dst.fpr(), bits),
            ValKind::F64 => (self.masm).move_float_constant(FloatWidth::F64, dst.fpr(), bits),
            ValKind::V128 => self.masm.move_vector_constant(dst.fpr(), bits.into()),
        }
    }

    pub(super) fn store_constant(&mut self, kind: ValKind, dst: Address, bits: u64) {
        match kind {
            ValKind::I32 | ValKind::F32 => self.masm.store32(dst, Untrusted(bits)),
            ValKind::I64 | ValKind::F64 | ValKind::Ref => self.masm.store64(dst, Untrusted(bits)),
            ValKind::V128 => unreachable!("vectors are not kept as constants"),
        }
    }

    pub(super) fn transfer(&mut self, kind: ValKind, dst: Address, src: Address) {
        match kind {
            ValKind::I32 | ValKind::F32 => self.masm.transfer32(dst, src),
            ValKind::I64 | ValKind::F64 | ValKind::Ref => self.masm.transfer64(dst, src),
            ValKind::V128 => {
                self.masm.load_vector(SCRATCH_FPR, src);
                self.masm.store_vector(dst, SCRATCH_FPR);
            }
        }
    }

    /// Where the value is now: not a [`Operand::Local`]. The register may be that of a local.
    #[inline(always)]
    pub(super) fn locate(&self, value: Operand) -> Operand {
        let Operand::Local(ty, local) = value else {
            return value;
        };
        let local = &self.locals[local as usize];
        match local.register {
            Some(reg) => Operand::Register(ty, reg),
            None => Operand::Slot(ty, local.offset),
        }
    }

    /// Takes no register.
    pub(super) fn store_operand(&mut self, dst: Address, value: Operand) {
        let kind = value.ty().kind();
        match self.locate(value) {
            Operand::Constant(_, bits) => self.store_constant(kind, dst, bits),
            Operand::Register(_, reg) => self.store(kind, dst, reg),
            Operand::Slot(_, offset) => self.transfer(kind, dst, self.frame_address(offset)),
            Operand::Local(..) => unreachable!(),
        }
    }

    pub(super) fn move_operand(&mut self, dst: Reg, value: Operand) {
        let kind = value.ty().kind();
        match self.locate(value) {
            Operand::Constant(_, bits) => self.move_constant(kind, dst, bits),
            Operand::Register(_, reg) if reg == dst => {}
            Operand::Register(_, reg) => self.copy(kind, dst, reg),
            Operand::Slot(_, offset) => self.load(kind, dst, self.frame_address(offset)),
            Operand::Local(..) => unreachable!(),
        }
    }
}
