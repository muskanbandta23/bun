//! What the registers hold: values of the operand stack, and copies of locals.
//!
//! A register is free, or belongs to one operand or one local. What an instruction pops or takes is locked until the
//! next instruction. A register that is both free and locked is the instruction's to overwrite.

use bun_engine_asm::prelude::*;
use bun_engine_asm::{Bank, Reg, RegOrImm, RegOrMem, Src};
use bun_engine_wasm_format::ValType;
use bun_engine_wasm_format::opcode::{LOCAL_SET, LOCAL_TEE};

use super::Function;
use super::memory::FirstMemory;
use super::operand::{Operand, bank, untrusted};
use crate::abi::INSTANCE;

#[derive(Clone, Copy)]
pub(super) struct Local {
    pub(super) ty: ValType,
    pub(super) offset: i32,
    pub(super) register: Option<Reg>,
    /// The register it was last in.
    pub(super) hint: Option<Reg>,
    /// The index of the last loop that was found to assign it before anything can read it.
    pub(super) dead_at_loop: u32,
}

impl Local {
    #[inline]
    pub(super) fn new(ty: ValType, offset: i32) -> Self {
        Self {
            ty,
            offset,
            register: None,
            hint: None,
            dead_at_loop: u32::MAX,
        }
    }
}

/// By the index of the register.
pub(super) type Owners = [u16; Reg::COUNT];

pub(super) struct Registers {
    pub(super) allocatable: RegisterSet,
    pub(super) free: RegisterSet,
    /// Those that belong to locals.
    pub(super) cached: RegisterSet,
    /// Those of `cached` whose local is not up to date in the frame.
    pub(super) dirty: RegisterSet,
    pub(super) locked: RegisterSet,
    pub(super) recent: RegisterSet,
    /// Set aside for the locals that the innermost loop keeps in registers.
    pub(super) pinned: RegisterSet,
    /// Those of `pinned` that may be dirty at a branch.
    pub(super) unsaved: RegisterSet,
    /// Stays when the local leaves the register.
    pub(super) local_of: Owners,
    /// The index of a copy of `local_of` as it is now.
    pub(super) snapshot: Option<u32>,
    pub(super) operand_of: [u32; Reg::COUNT],
}

const ALLOCATABLE: RegisterSet = RegisterSet::of(&abi::TEMPORARY_GPRS, &[])
    .union(RegisterSet::of(&abi::SAVED_GPRS, &[]))
    .union(abi::PINNED)
    .union(abi::ALL.in_bank(Bank::Fpr))
    .difference(RegisterSet::of(&[INSTANCE], &abi::SCRATCH_FPRS));

impl Registers {
    pub(super) fn new(first_memory: FirstMemory) -> Self {
        let allocatable = ALLOCATABLE - first_memory.registers();
        Self {
            allocatable,
            free: allocatable,
            cached: RegisterSet::EMPTY,
            dirty: RegisterSet::EMPTY,
            locked: RegisterSet::EMPTY,
            recent: RegisterSet::EMPTY,
            pinned: RegisterSet::EMPTY,
            unsaved: RegisterSet::EMPTY,
            local_of: [0; Reg::COUNT],
            snapshot: None,
            operand_of: [0; Reg::COUNT],
        }
    }

    /// Those that belong to operands.
    #[inline(always)]
    pub(super) fn operands(&self) -> RegisterSet {
        self.allocatable - self.free - self.cached
    }

    #[inline(always)]
    pub(super) fn local_in(&self, reg: Reg) -> u32 {
        self.local_of[reg.index()].into()
    }

    /// Operands are taken from the other end.
    #[inline]
    pub(super) fn best_for_local(candidates: RegisterSet) -> Option<Reg> {
        let kept = candidates & abi::CALLEE_SAVED;
        let bits = if kept.is_empty() { candidates } else { kept }.bits();
        Some(Reg::from_index(bits.checked_ilog2()? as usize))
    }
}

/// An integer where an instruction can read it.
#[derive(Clone, Copy)]
pub(super) enum Source {
    Register(Gpr),
    Constant(u64),
    Frame(i32),
}

/// More lazy operands than this are not searched again and again.
const LONGEST_SEARCH: u32 = 16;

impl Function<'_> {
    // The operand stack

    #[inline(always)]
    pub(super) fn height(&self) -> u32 {
        self.stack.len() as u32
    }

    /// A constant or a local.
    #[inline(always)]
    pub(super) fn push_lazy(&mut self, value: Operand) {
        self.lowest_lazy = self.lowest_lazy.min(self.height());
        self.stack.push(value);
    }

    /// The register is free.
    #[inline(always)]
    pub(super) fn push_register(&mut self, ty: ValType, reg: impl Into<Reg>) {
        let reg = reg.into();
        debug_assert!(self.registers.free.contains(reg));
        self.registers.free.remove(reg);
        self.registers.operand_of[reg.index()] = self.height();
        self.stack.push(Operand::Register(ty, reg));
    }

    pub(super) fn push_slot(&mut self, ty: ValType) {
        let offset = self.frame.operand(self.height());
        self.stack.push(Operand::Slot(ty, offset));
    }

    /// Puts back what was just popped.
    pub(super) fn push(&mut self, value: Operand) {
        match value {
            Operand::Register(ty, reg) => self.push_register(ty, reg),
            Operand::Slot(ty, _) => self.push_slot(ty),
            Operand::Constant(..) | Operand::Local(..) => self.push_lazy(value),
        }
    }

    #[inline(always)]
    pub(super) fn pop(&mut self) -> Operand {
        let value = self.stack.pop().expect("the code was validated");
        if let Operand::Register(_, reg) = value {
            self.registers.free.insert(reg);
            self.registers.locked.insert(reg);
        }
        value
    }

    #[inline(always)]
    pub(super) fn pop_gpr(&mut self) -> Gpr {
        let value = self.pop();
        self.gpr(value)
    }

    #[inline(always)]
    pub(super) fn pop_fpr(&mut self) -> Fpr {
        let value = self.pop();
        self.fpr(value)
    }

    #[inline(always)]
    pub(super) fn top(&self) -> Operand {
        *self.stack.last().expect("the code was validated")
    }

    /// Leaves the operand at the top in a register of its own, which is locked.
    pub(super) fn own_top(&mut self) -> Reg {
        let value = self.pop();
        let reg = self.writable(value);
        self.push_register(value.ty(), reg);
        reg
    }

    /// Puts the operands below `end` in their slots.
    #[inline(always)]
    pub(super) fn settle(&mut self, end: u32) {
        if end != 0 && (self.lowest_lazy < end || !self.registers.operands().is_empty()) {
            self.spill_below(self.registers.allocatable, end);
            self.settle_lazy(end);
        }
    }

    fn settle_lazy(&mut self, end: u32) {
        for position in self.lowest_lazy..end {
            let value = self.stack[position as usize];
            if let Operand::Constant(ty, _) | Operand::Local(ty, _) = value {
                let offset = self.frame.operand(position);
                self.store_operand(self.frame_address(offset), value);
                self.stack[position as usize] = Operand::Slot(ty, offset);
            }
        }
        self.lowest_lazy = self.lowest_lazy.max(end);
    }

    /// Those of the operands below `end` that are in `registers`.
    pub(super) fn spill_below(&mut self, registers: RegisterSet, end: u32) {
        for reg in self.registers.operands() & registers {
            if self.registers.operand_of[reg.index()] < end {
                self.spill(reg);
            }
        }
    }

    fn spill(&mut self, reg: Reg) {
        let position = self.registers.operand_of[reg.index()];
        let ty = self.stack[position as usize].ty();
        let offset = self.frame.operand(position);
        self.store(ty.kind(), self.frame_address(offset), reg);
        self.stack[position as usize] = Operand::Slot(ty, offset);
        self.registers.free.insert(reg);
    }

    // Reading what was popped

    /// Not to be written unless it is free.
    #[inline(always)]
    pub(super) fn register(&mut self, value: Operand) -> Reg {
        match value {
            Operand::Register(_, reg) => reg,
            Operand::Local(_, local) if let Some(reg) = self.locals[local as usize].register => {
                self.registers.locked.insert(reg);
                reg
            }
            _ => self.bring(value),
        }
    }

    #[inline(always)]
    pub(super) fn gpr(&mut self, value: Operand) -> Gpr {
        self.register(value).gpr()
    }

    #[inline(always)]
    pub(super) fn fpr(&mut self, value: Operand) -> Fpr {
        self.register(value).fpr()
    }

    #[inline(never)]
    fn bring(&mut self, value: Operand) -> Reg {
        let Operand::Local(_, local) = value else {
            return self.writable(value);
        };
        let reg = self.take_for_local(local);
        let Local { ty, offset, .. } = self.locals[local as usize];
        self.load(ty.kind(), reg, self.frame_address(offset));
        self.bind(local, reg);
        reg
    }

    pub(super) fn writable(&mut self, value: Operand) -> Reg {
        if let Operand::Register(_, reg) = value {
            return reg;
        }
        self.lock(value);
        let reg = self.take(bank(value.ty().kind()));
        self.move_operand(reg, value);
        reg
    }

    /// Keeps a local in its register.
    pub(super) fn lock(&mut self, value: Operand) {
        if let Operand::Register(_, reg) = self.locate(value) {
            self.registers.locked.insert(reg);
        }
    }

    /// Nothing is put out of its register for it.
    #[inline(always)]
    pub(super) fn source(&mut self, value: Operand) -> Source {
        match value {
            Operand::Constant(_, bits) => Source::Constant(bits),
            Operand::Register(_, reg) => Source::Register(reg.gpr()),
            Operand::Slot(_, offset) => Source::Frame(offset),
            Operand::Local(_, index) => {
                let local = &self.locals[index as usize];
                if let Some(reg) = local.register {
                    self.registers.locked.insert(reg);
                    Source::Register(reg.gpr())
                } else if self.unused(Bank::Gpr).len() > 1 || self.has_own_register(index) {
                    Source::Register(self.bring(value).gpr())
                } else {
                    Source::Frame(local.offset)
                }
            }
        }
    }

    #[inline(always)]
    pub(super) fn src(&self, source: Source) -> Src {
        match source {
            Source::Register(reg) => reg.into(),
            Source::Constant(bits) => Untrusted(bits).into(),
            Source::Frame(offset) => self.frame_address(offset).into(),
        }
    }

    /// `source` is no constant.
    #[inline(always)]
    pub(super) fn reg_or_mem(&self, source: Source) -> RegOrMem {
        match source {
            Source::Register(reg) => reg.into(),
            Source::Frame(offset) => self.frame_address(offset).into(),
            Source::Constant(_) => unreachable!(),
        }
    }

    #[inline(always)]
    pub(super) fn reg_or_imm(&mut self, value: Operand) -> RegOrImm {
        match value {
            Operand::Constant(_, bits) => untrusted(bits),
            _ => self.gpr(value).into(),
        }
    }

    // Taking registers

    #[inline(always)]
    fn unused(&self, bank: Bank) -> RegisterSet {
        let registers = &self.registers;
        (registers.free - registers.locked - registers.pinned).in_bank(bank)
    }

    #[inline(always)]
    pub(super) fn take(&mut self, bank: Bank) -> Reg {
        let reg = match self.unused(bank).first() {
            Some(reg) => reg,
            None => self.make_room_for_operand(bank),
        };
        self.registers.locked.insert(reg);
        reg
    }

    #[inline(always)]
    pub(super) fn take_gpr(&mut self) -> Gpr {
        self.take(Bank::Gpr).gpr()
    }

    /// `operand`, which was popped, if nothing else needs it, and otherwise another of its bank.
    #[inline(always)]
    pub(super) fn reuse(&mut self, operand: impl Into<Reg>) -> Reg {
        let operand = operand.into();
        if self.registers.free.contains(operand) {
            operand
        } else {
            self.take(operand.bank())
        }
    }

    /// For the result of an instruction that has read its operands by the time it writes. `next` is the offset of the
    /// instruction that follows.
    #[inline(always)]
    pub(super) fn result(&mut self, next: usize, bank: Bank) -> Reg {
        match self.register_assigned_at(next) {
            Some(reg) => reg,
            None => self.take(bank),
        }
    }

    /// Like [`Self::result`] and [`Self::reuse`].
    #[inline(always)]
    pub(super) fn result_reusing(&mut self, next: usize, operand: impl Into<Reg>) -> Reg {
        match self.register_assigned_at(next) {
            Some(reg) => reg,
            None => self.reuse(operand),
        }
    }

    #[inline(always)]
    fn register_assigned_at(&mut self, next: usize) -> Option<Reg> {
        match *self.bytes.get(next..)?.first_chunk()? {
            [LOCAL_SET | LOCAL_TEE, low, high] => self.register_to_assign(low, high),
            _ => None,
        }
    }

    /// The register of a local whose value is not read any more, but for the operands that were popped.
    #[inline(never)]
    fn register_to_assign(&mut self, low: u8, high: u8) -> Option<Reg> {
        let local = match (low, high) {
            (0..0x80, _) => u32::from(low),
            (_, 0..0x80) => u32::from(low & 0x7f) | u32::from(high) << 7,
            _ => return None,
        };
        let reg = match self.locals[local as usize].register {
            Some(_) if self.may_be_on_stack(local) => return None,
            Some(reg) => {
                self.unbind(reg);
                reg
            }
            None => self.free_hint(local)?,
        };
        self.registers.locked.insert(reg);
        Some(reg)
    }

    fn may_be_on_stack(&self, local: u32) -> bool {
        let lazy = self.stack.get(self.lowest_lazy as usize..).unwrap_or(&[]);
        lazy.len() > LONGEST_SEARCH as usize
            || (lazy.iter()).any(|value| matches!(*value, Operand::Local(_, own) if own == local))
    }

    /// The register that the local was last in, if it is free and not set aside for another.
    #[inline]
    fn free_hint(&self, local: u32) -> Option<Reg> {
        let registers = &self.registers;
        self.locals[local as usize].hint.filter(|&hint| {
            registers.free.contains(hint)
                && (!registers.pinned.contains(hint) || registers.local_in(hint) == local)
        })
    }

    /// Whether one is set aside for it and can be had.
    fn has_own_register(&self, local: u32) -> bool {
        let available = self.registers.pinned - self.registers.locked;
        self.free_hint(local)
            .is_some_and(|hint| available.contains(hint))
    }

    fn take_for_local(&mut self, local: u32) -> Reg {
        let bank = bank(self.locals[local as usize].ty.kind());
        let hint = self.free_hint(local);
        let reg = hint
            .filter(|&hint| !self.registers.locked.contains(hint))
            .or_else(|| Registers::best_for_local(self.unused(bank)))
            .unwrap_or_else(|| self.make_room(bank));
        self.registers.locked.insert(reg);
        reg
    }

    #[inline(never)]
    fn make_room_for_operand(&mut self, bank: Bank) -> Reg {
        let vacant = (self.registers.free - self.registers.locked).in_bank(bank);
        vacant.first().unwrap_or_else(|| self.make_room(bank))
    }

    /// Returns a free register that is not locked.
    #[inline(never)]
    fn make_room(&mut self, bank: Bank) -> Reg {
        let registers = &mut self.registers;
        let candidates = (registers.allocatable - registers.locked).in_bank(bank);
        let locals = (candidates & registers.cached) - registers.pinned;
        let mut stale = locals - registers.recent;
        if stale.is_empty() {
            registers.recent -= candidates;
            stale = locals;
        }
        let deepest_operand = || {
            (candidates & registers.operands())
                .into_iter()
                .min_by_key(|reg| registers.operand_of[reg.index()])
        };
        let reg = (stale - registers.dirty)
            .first()
            .or_else(|| stale.first())
            .or_else(|| (candidates & registers.free).first())
            .or_else(deepest_operand)
            .or_else(|| (candidates - registers.dirty).first())
            .or_else(|| candidates.first())
            .expect("an instruction locks few registers");
        self.evict(reg);
        reg
    }

    /// What is in the register goes to the frame.
    fn evict(&mut self, reg: Reg) {
        if self.registers.cached.contains(reg) {
            if self.registers.dirty.contains(reg) {
                self.write_back(reg);
            }
            self.unbind(reg);
        } else if !self.registers.free.contains(reg) {
            self.spill(reg);
        }
    }

    /// Frees and locks registers that an instruction is going to overwrite. Operands that were popped stay.
    pub(super) fn claim(&mut self, registers: RegisterSet) {
        self.registers.locked |= registers;
        for reg in registers - self.registers.free {
            let Some(other) = self.unused(reg.bank()).first() else {
                self.evict(reg);
                continue;
            };
            if self.registers.cached.contains(reg) {
                let local = self.registers.local_in(reg);
                let is_dirty = self.registers.dirty.contains(reg);
                self.copy(self.locals[local as usize].ty.kind(), other, reg);
                self.unbind(reg);
                self.bind(local, other);
                if is_dirty {
                    self.registers.dirty.insert(other);
                }
            } else {
                let position = self.registers.operand_of[reg.index()];
                let ty = self.stack[position as usize].ty();
                self.copy(ty.kind(), other, reg);
                self.registers.free.insert(reg);
                self.give_to_operand(position, ty, other);
            }
        }
    }

    /// For an integer that was popped before `registers` were claimed.
    pub(super) fn gpr_outside(&mut self, value: Operand, registers: RegisterSet) -> Gpr {
        match value {
            Operand::Register(ty, reg) if registers.contains(reg) => {
                let other = self.take(Bank::Gpr);
                self.copy(ty.kind(), other, reg);
                other.gpr()
            }
            _ => self.gpr(value),
        }
    }

    fn give_to_operand(&mut self, position: u32, ty: ValType, reg: Reg) {
        self.registers.free.remove(reg);
        self.registers.operand_of[reg.index()] = position;
        self.stack[position as usize] = Operand::Register(ty, reg);
    }

    // Locals

    /// The register is free and the local has none.
    #[inline(always)]
    pub(super) fn bind(&mut self, local: u32, reg: Reg) {
        let registers = &mut self.registers;
        debug_assert!(registers.free.contains(reg));
        registers.free.remove(reg);
        registers.cached.insert(reg);
        if registers.local_in(reg) != local {
            registers.local_of[reg.index()] = local as u16;
            registers.snapshot = None;
        }
        let local = &mut self.locals[local as usize];
        debug_assert!(local.register.is_none());
        (local.register, local.hint) = (Some(reg), Some(reg));
    }

    /// Stores nothing.
    #[inline(always)]
    pub(super) fn unbind(&mut self, reg: Reg) {
        let registers = &mut self.registers;
        debug_assert!(registers.cached.contains(reg));
        registers.free.insert(reg);
        registers.cached.remove(reg);
        registers.dirty.remove(reg);
        self.locals[registers.local_in(reg) as usize].register = None;
    }

    #[inline]
    pub(super) fn unbind_all(&mut self, registers: RegisterSet) {
        for reg in registers {
            self.unbind(reg);
        }
    }

    #[inline]
    fn write_back(&mut self, reg: Reg) {
        let Local { ty, offset, .. } = self.locals[self.registers.local_in(reg) as usize];
        self.store(ty.kind(), self.frame_address(offset), reg);
        self.registers.dirty.remove(reg);
    }

    #[inline]
    pub(super) fn write_back_all(&mut self, registers: RegisterSet) {
        for reg in registers {
            self.write_back(reg);
        }
    }

    #[inline(always)]
    pub(super) fn set_local(&mut self, local: u32) {
        let value = self.pop();
        if matches!(value, Operand::Local(_, source) if source == local) {
            return;
        }
        if self.lowest_lazy < self.height() {
            self.detach(local);
        }
        match (value, self.locals[local as usize].register) {
            (Operand::Register(_, reg), None) if !self.has_own_register(local) => {
                self.bind(local, reg);
                self.registers.dirty.insert(reg);
            }
            (_, old) => self.assign(local, old, value),
        }
    }

    #[inline(never)]
    fn assign(&mut self, local: u32, old: Option<Reg>, value: Operand) {
        let reg = match (value, old) {
            (Operand::Register(_, reg), Some(old)) if !self.registers.pinned.contains(old) => {
                self.unbind(old);
                self.bind(local, reg);
                reg
            }
            (_, Some(old)) => {
                self.move_operand(old, value);
                old
            }
            (Operand::Constant(ty, bits), None)
                if self.unused(bank(ty.kind())).len() < 2 && !self.has_own_register(local) =>
            {
                let offset = self.locals[local as usize].offset;
                return self.store_constant(ty.kind(), self.frame_address(offset), bits);
            }
            (_, None) => {
                self.lock(value);
                let reg = self.take_for_local(local);
                self.move_operand(reg, value);
                self.bind(local, reg);
                reg
            }
        };
        self.registers.locked.insert(reg);
        self.registers.dirty.insert(reg);
    }

    /// Gives the operands that stand for the local, which is about to be assigned, a place of their own.
    #[inline(never)]
    fn detach(&mut self, local: u32) {
        let height = self.height();
        if height - self.lowest_lazy > LONGEST_SEARCH {
            return self.settle_lazy(height);
        }
        let mut first = None;
        for position in self.lowest_lazy..height {
            let value = self.stack[position as usize];
            let Operand::Local(ty, own) = value else {
                continue;
            };
            if own != local {
                continue;
            }
            let reg = match (first, self.locals[local as usize].register) {
                (None, Some(reg)) => {
                    self.unbind(reg);
                    self.registers.locked.insert(reg);
                    reg
                }
                (None, None) => self.writable(value),
                (Some(first), _) => {
                    let reg = self.take(bank(ty.kind()));
                    self.copy(ty.kind(), reg, first);
                    reg
                }
            };
            self.give_to_operand(position, ty, reg);
            if first.is_some() {
                self.registers.locked.remove(reg);
            }
            first = first.or(Some(reg));
        }
    }
}
