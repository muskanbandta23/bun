//! Comparisons that a branch or a `select` uses without a boolean in between.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, RegOrImm};
use bun_engine_wasm_format::opcode::{BR_IF, I32_EQZ, IF, SELECT, SELECT_TYPED};
use bun_engine_wasm_format::{ValKind, ValType};

use super::Function;
use super::operand::{Operand, untrusted};
use super::registers::Source;

#[derive(Clone, Copy)]
pub(super) enum Condition {
    /// The left side is no constant, and at most one is in the frame.
    Compare(Width, Cond, Source, Source),
    /// Of a value, which is no constant, with a mask, which is not in the frame.
    Test(Width, TestCond, Source, Source),
    Float(FloatWidth, FloatCond, Fpr, Fpr),
}

/// What decides where control goes.
pub(super) enum Decision {
    Known(bool),
    Unknown(Condition),
}

/// A condition that stands in for the operand at the top of the stack until the next instruction.
#[derive(Clone, Copy)]
pub(super) struct Pending {
    condition: Condition,
    locked: RegisterSet,
}

const ALL_BITS: Source = Source::Constant(u64::MAX);

impl Condition {
    #[inline]
    pub(super) fn inverted(self) -> Self {
        match self {
            Self::Compare(width, cond, a, b) => Self::Compare(width, cond.inverted(), a, b),
            Self::Test(width, cond, a, b) => Self::Test(width, cond.inverted(), a, b),
            Self::Float(width, cond, a, b) => Self::Float(width, cond.inverted(), a, b),
        }
    }
}

#[inline(always)]
fn mask(source: Source) -> RegOrImm {
    match source {
        Source::Register(reg) => reg.into(),
        Source::Constant(bits) if bits == u64::MAX || bits <= 0xffff => RegOrImm::Imm(bits as i64),
        Source::Constant(bits) => untrusted(bits),
        Source::Frame(_) => unreachable!(),
    }
}

impl Function<'_> {
    /// Whether the instruction at `next` only asks if its operand of 32 bits is zero.
    #[inline(always)]
    pub(super) fn is_tested_at(&self, next: usize) -> bool {
        matches!(
            self.bytes.get(next),
            Some(&(BR_IF | IF | I32_EQZ | SELECT | SELECT_TYPED))
        )
    }

    /// The next instruction takes it.
    #[inline(always)]
    pub(super) fn defer(&mut self, condition: Condition) {
        self.pending = Some(Pending {
            condition,
            locked: self.registers.locked,
        });
    }

    #[inline(always)]
    pub(super) fn push_condition(&mut self, condition: Condition, next: usize) {
        if self.is_tested_at(next) {
            self.defer(condition);
        } else {
            let result = self.boolean(condition, next);
            self.push_register(ValType::I32, result);
        }
    }

    /// Whether the integer at the top of the stack is not zero.
    #[inline(always)]
    pub(super) fn pop_decision(&mut self, width: Width) -> Decision {
        if let Some(pending) = self.pending.take() {
            self.registers.locked |= pending.locked;
            return Decision::Unknown(pending.condition);
        }
        match self.pop() {
            Operand::Constant(_, bits) => Decision::Known(bits != 0),
            value => {
                let value = self.source(value);
                Decision::Unknown(Condition::Test(width, TestCond::NonZero, value, ALL_BITS))
            }
        }
    }

    pub(super) fn branch(&mut self, condition: Condition, target: Label) {
        match condition {
            Condition::Compare(width, cond, lhs, rhs) => {
                let (lhs, rhs) = (self.reg_or_mem(lhs), self.src(rhs));
                self.masm.branch(width, cond, lhs, rhs, target);
            }
            Condition::Test(width, cond, value, bits) => {
                let value = self.reg_or_mem(value);
                self.masm
                    .branch_test(width, cond, value, mask(bits), target);
            }
            Condition::Float(width, cond, lhs, rhs) => {
                self.masm.branch_float(width, cond, lhs, rhs, target);
            }
        }
    }

    #[inline(never)]
    fn boolean(&mut self, condition: Condition, next: usize) -> Gpr {
        let dst = match condition {
            Condition::Compare(_, _, Source::Register(operand), _)
            | Condition::Test(_, _, Source::Register(operand), _) => {
                self.result_reusing(next, operand).gpr()
            }
            _ => self.result(next, Bank::Gpr).gpr(),
        };
        match condition {
            Condition::Compare(width, cond, lhs, rhs) => {
                let (lhs, rhs) = (self.reg_or_mem(lhs), self.src(rhs));
                self.masm.compare(width, cond, dst, lhs, rhs);
            }
            Condition::Test(width, cond, value, bits) => {
                let value = self.reg_or_mem(value);
                self.masm.test(width, cond, dst, value, mask(bits));
            }
            Condition::Float(width, cond, lhs, rhs) => {
                self.masm.compare_float(width, cond, dst, lhs, rhs);
            }
        }
        dst
    }

    fn source_in_register(&mut self, width: Width, source: Source) -> Gpr {
        match source {
            Source::Register(reg) => reg,
            Source::Frame(offset) => self.gpr(Operand::Slot(ValType::int(width), offset)),
            Source::Constant(_) => unreachable!(),
        }
    }

    pub(super) fn compare_ints(&mut self, width: Width, cond: Cond, next: usize) {
        let (rhs, lhs) = (self.pop(), self.pop());
        let (cond, lhs, rhs) = match (lhs.constant(), rhs.constant()) {
            (Some(lhs), Some(rhs)) => {
                let holds = match width {
                    Width::W32 => cond.holds32(lhs as i32, rhs as i32),
                    Width::W64 => cond.holds64(lhs as i64, rhs as i64),
                };
                return self.push_lazy(Operand::int(Width::W32, holds.into()));
            }
            (Some(_), None) => (cond.commuted(), rhs, lhs),
            (None, _) => (cond, lhs, rhs),
        };
        let rhs = self.source(rhs);
        let lhs = match rhs {
            Source::Frame(_) => Source::Register(self.gpr(lhs)),
            _ => self.source(lhs),
        };
        self.push_condition(Condition::Compare(width, cond, lhs, rhs), next);
    }

    pub(super) fn select_value(&mut self, ty: Option<ValType>, next: usize) {
        let decision = self.pop_decision(Width::W32);
        let condition = match decision {
            Decision::Known(holds) => return self.select_known(holds),
            Decision::Unknown(condition) => condition,
        };
        let (or, then) = (self.pop(), self.pop());
        let ty = ty.unwrap_or_else(|| then.ty());
        self.lock(then);
        let (or, then) = (self.register(or), self.register(then));
        let reusable = if self.registers.free.contains(or) {
            or
        } else {
            then
        };
        let kind = ty.kind();
        match condition {
            Condition::Compare(width, cond, lhs, rhs) if or.bank() == Bank::Gpr => {
                let (cond, lhs, rhs) = match rhs {
                    Source::Frame(_) => (cond.commuted(), rhs, lhs),
                    _ => (cond, lhs, rhs),
                };
                let lhs = self.source_in_register(width, lhs);
                let rhs = match rhs {
                    Source::Register(reg) => reg.into(),
                    Source::Constant(bits) => untrusted(bits),
                    Source::Frame(_) => unreachable!(),
                };
                let dst = self.result_reusing(next, reusable);
                (self.masm).select(width, cond, dst.gpr(), lhs, rhs, then.gpr(), or.gpr());
                self.push_register(ty, dst);
            }
            Condition::Float(width, cond, lhs, rhs) if kind != ValKind::V128 => {
                let dst = self.result_reusing(next, reusable);
                let masm = &mut self.masm;
                if dst.bank() == Bank::Gpr {
                    masm.select_on_float(width, cond, dst.gpr(), lhs, rhs, then.gpr(), or.gpr());
                } else {
                    let (dst, then, or) = (dst.fpr(), then.fpr(), or.fpr());
                    masm.select_float_on_float(width, cond, dst, lhs, rhs, then, or);
                }
                self.push_register(ty, dst);
            }
            _ => {
                let (width, cond, value, bits) = match condition {
                    Condition::Test(width, cond, value, bits) => {
                        (width, cond, self.source_in_register(width, value), bits)
                    }
                    _ => {
                        let value = self.boolean(condition, self.offset);
                        (Width::W32, TestCond::NonZero, value, ALL_BITS)
                    }
                };
                let dst = self.result_reusing(next, reusable);
                let (masm, bits) = (&mut self.masm, mask(bits));
                match kind {
                    ValKind::V128 => {
                        let (dst, then, or) = (dst.fpr(), then.fpr(), or.fpr());
                        masm.select_vector_test(width, cond, dst, value, bits, then, or);
                    }
                    ValKind::F32 | ValKind::F64 => {
                        let (dst, then, or) = (dst.fpr(), then.fpr(), or.fpr());
                        masm.select_float_test(width, cond, dst, value, bits, then, or);
                    }
                    ValKind::I32 | ValKind::I64 | ValKind::Ref => {
                        masm.select_test(width, cond, dst.gpr(), value, bits, then.gpr(), or.gpr());
                    }
                }
                self.push_register(ty, dst);
            }
        }
    }

    fn select_known(&mut self, holds: bool) {
        let or = self.pop();
        if holds {
            return;
        }
        self.pop();
        match or {
            Operand::Slot(ty, _) => {
                let reg = self.writable(or);
                self.push_register(ty, reg);
            }
            _ => self.push(or),
        }
    }
}
