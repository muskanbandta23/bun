//! Addressing modes.

use bun_engine_asm::arm64::{AccessSize, ArithmeticImm, FP, IndexExtend, Mem as Encoded};

use super::instruction::{Amode, Inst};
use crate::ir::{Binary, Op, Type, Value};
use crate::machine::{Lowering, Place, Role};
use bun_engine_asm::Width;

const MOST_STEPS: u32 = 8;

/// Constants from which other addresses may be within reach.
#[derive(Default)]
pub(crate) struct Neighbours {
    bases: [Option<i64>; 4],
    oldest: usize,
}

impl Neighbours {
    fn base_for(&mut self, address: i64, size: AccessSize) -> i64 {
        let reaches = |&base: &i64| Encoded::fits_offset(size, address.wrapping_sub(base));
        let known = self.bases.iter().flatten().copied().find(reaches);
        known.unwrap_or_else(|| {
            let rounded = [0xffff, 0xfff, 0xff].map(|low| address & !low);
            let base = rounded.into_iter().find(reaches).unwrap();
            self.bases[self.oldest] = Some(base);
            self.oldest = (self.oldest + 1) % self.bases.len();
            base
        })
    }
}

#[derive(Clone, Copy)]
pub(super) struct AddressParts {
    pub(super) mode: Amode,
    pub(super) base: Value,
    pub(super) index: Value,
}

impl AddressParts {
    fn new(mode: Amode, base: Value) -> Self {
        let index = Value::NONE;
        Self { mode, base, index }
    }
}

/// One instruction can add what the access cannot.
fn can_be_split(size: AccessSize, offset: i64) -> bool {
    ArithmeticImm::new((offset & !0xfff).unsigned_abs()).is_some()
        && Encoded::fits_offset(size, offset & 0xfff)
}

impl Lowering<'_, Inst> {
    pub(super) fn frame_offset(&self, value: Value) -> Option<i32> {
        match self.op(value) {
            Op::SlotAddress(slot) => Some(self.slot_offset(slot)),
            Op::Pinned(FP) => Some(0),
            _ => None,
        }
    }

    /// `value` as another plus a constant.
    fn displaced(&self, value: Value) -> Option<(Value, i64)> {
        match self.op(value) {
            Op::Binary(Binary::Add, [rest, constant]) => Some((rest, self.int_constant(constant)?)),
            Op::Binary(Binary::Sub, [rest, constant]) => {
                Some((rest, self.int_constant(constant)?.wrapping_neg()))
            }
            _ => None,
        }
        .filter(|_| !self.is_materialized(value))
    }

    /// How an access extends and scales an index by itself.
    fn folded_index(&self, value: Value, size: AccessSize) -> Option<(Amode, Value)> {
        if self.is_materialized(value) {
            return None;
        }
        self.match_field(value, |field| {
            let extend = if self.is_bare(field, Width::W64) {
                IndexExtend::Lsl
            } else if (field.first, field.count) != (0, 32) {
                return None;
            } else if field.is_signed {
                IndexExtend::Sxtw
            } else {
                IndexExtend::Uxtw
            };
            let is_scaled = field.to != 0;
            (!is_scaled || field.to == size.log2_bytes())
                .then_some((Amode::Indexed(extend, is_scaled), field.source))
        })
    }

    fn indexed(&self, base: Value, index: Value, size: AccessSize) -> AddressParts {
        let folded = |base, index| Some((base, self.folded_index(index, size)?));
        let (base, (mode, index)) = folded(base, index)
            .or_else(|| folded(index, base))
            .unwrap_or((base, (Amode::Indexed(IndexExtend::Lsl, false), index)));
        AddressParts { mode, base, index }
    }

    pub(super) fn address_parts(
        &mut self,
        mut address: Value,
        offset: i32,
        size: AccessSize,
    ) -> AddressParts {
        let mut offset = i64::from(offset);
        for _ in 0..MOST_STEPS {
            let Some((rest, constant)) = self.displaced(address) else {
                break;
            };
            let sum = offset.wrapping_add(constant);
            let is_worth_sharing = self.use_count(address) > 1
                && Encoded::fits_offset(size, offset)
                && !Encoded::fits_offset(size, sum)
                && !self.op(rest).is_floating();
            if is_worth_sharing {
                break;
            }
            (address, offset) = (rest, sum);
        }
        if let Some(constant) = self.int_constant(address) {
            let absolute = constant.wrapping_add(offset);
            let base = self.selection.base_for(absolute, size);
            let mode = Amode::Offset(absolute.wrapping_sub(base) as i32);
            return AddressParts::new(mode, self.intern(Type::I64, base as u64));
        }
        if let Some(slot) = self.frame_offset(address)
            && let Ok(offset) = i32::try_from(offset.wrapping_add(slot.into()))
        {
            return AddressParts::new(Amode::Frame(offset), Value::NONE);
        }
        if offset == 0 {
            return match self.op(address) {
                Op::Binary(Binary::Add, [lhs, rhs]) if self.dissolves(address) => {
                    self.indexed(lhs, rhs, size)
                }
                _ => AddressParts::new(Amode::Offset(0), address),
            };
        }
        let takes_no_register = Encoded::fits_offset(size, offset)
            || !self.is_in_loop()
                && can_be_split(size, offset)
                && self.folded_index(address, size).is_none();
        if takes_no_register {
            return AddressParts::new(Amode::Offset(offset as i32), address);
        }
        let constant = self.intern(Type::I64, offset as u64);
        self.indexed(constant, address, size)
    }

    /// For the instructions that take nothing but a register.
    pub(super) fn parts_without_index(&mut self, address: Value, offset: i32) -> AddressParts {
        let parts = self.address_parts(address, offset, AccessSize::S8);
        match parts.mode {
            Amode::Indexed(..) => AddressParts::new(Amode::Offset(offset), address),
            _ => parts,
        }
    }

    pub(super) fn use_parts(&mut self, parts: AddressParts, role: Role) -> Amode {
        for part in [parts.base, parts.index] {
            if !part.is_none() {
                self.use_value(part, role, Place::Register);
            }
        }
        parts.mode
    }
}
