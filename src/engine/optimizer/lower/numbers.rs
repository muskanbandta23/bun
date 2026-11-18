//! Between the tagged form of a number and the bits that arithmetic works on.

use bun_engine_asm::{FloatCond, Sign, abi};
use bun_engine_codegen::ir::{Block, Op, Type, Unary, Value as Ssa};
use bun_engine_vm::Value;

use super::Lowering;
use super::runtime_calls::Call;
use crate::builder::Built;
use crate::graph::ExitKind;

/// Added to an integer of at most 52 bits, it puts the integer in the low bits of the double.
const INTEGER_IN_LOW_BITS: f64 = (3u64 << 51) as f64;
const SIGN_BIT: i64 = 1 << 63;
/// The first magnitude that no i64 holds.
const TWO_TO_THE_63: f64 = (1u64 << 63) as f64;

impl Lowering<'_> {
    pub(super) fn box_int32(&mut self, int32: Ssa) -> Ssa {
        let wide = self.ir().zero_extend(int32);
        let tag = self.tag(abi::NUMBER_TAG);
        self.ir().or(wide, tag)
    }

    /// An integer of 52 bits is an int32 where it is one and a double otherwise, as `Value::number` has it.
    pub(super) fn box_int52(&mut self) -> Ssa {
        let wide = self.operand(0);
        let narrow = self.ir().wrap(wide);
        let back = self.ir().sign_extend(narrow);
        let fits = self.ir().equal(back, wide);
        let as_int32 = self.box_int32(narrow);
        let number = self.int_to_double(wide);
        let as_double = self.double_bits(number, false);
        self.ir().select(fits, as_int32, as_double)
    }

    pub(super) fn box_double(&mut self, number: Ssa) -> Ssa {
        let may_be_nan = !self.is_never_nan(number);
        self.double_bits(number, may_be_nan)
    }

    fn double_bits(&mut self, number: Ssa, may_be_nan: bool) -> Ssa {
        let number = if may_be_nan {
            self.ir().unary(Unary::CanonicalizeNan, number)
        } else {
            number
        };
        let bits = self.ir().bitcast(number);
        self.ir().add(bits, Value::DOUBLE_OFFSET as i64)
    }

    /// Whether the instruction that gives the double cannot give a NaN.
    fn is_never_nan(&self, number: Ssa) -> bool {
        match *self.graph.function.op(number) {
            Op::ConstF64(bits) => !f64::from_bits(bits.get()).is_nan(),
            Op::Unary(Unary::IntToFloat(_) | Unary::CanonicalizeNan, _) => true,
            _ => false,
        }
    }

    pub(super) fn box_boolean(&mut self) -> Ssa {
        let boolean = self.operand(0);
        let wide = self.ir().zero_extend(boolean);
        self.ir().or(wide, Value::FALSE.to_bits() as i64)
    }

    fn int_to_double(&mut self, integer: Ssa) -> Ssa {
        self.ir()
            .convert(Unary::IntToFloat(Sign::Signed), Type::F64, integer)
    }

    /// The double that the bits of a tagged double hold.
    fn double_in(&mut self, block: Block, number: Ssa) -> Ssa {
        let bits = self.at_end(block).sub(number, Value::DOUBLE_OFFSET as i64);
        self.at_end(block).bitcast(bits)
    }

    /// The double that a tagged int32 holds.
    fn int32_in(&mut self, block: Block, number: Ssa) -> Ssa {
        let narrow = self.at_end(block).wrap(number);
        self.at_end(block)
            .convert(Unary::IntToFloat(Sign::Signed), Type::F64, narrow)
    }

    /// The double that a tagged number holds. `is_mostly_int32` says which way is worth the fall-through.
    pub(super) fn number_to_double(&mut self, is_mostly_int32: bool) -> Built {
        let number = self.operand(0);
        let block = self.block();
        if is_mostly_int32 {
            let whole = self.int32_in(block, number);
            let is_double = self.ir().below(number, Value::NUMBER_TAG as i64);
            self.branch_out(is_double, whole, false, |lowering, other| {
                lowering.double_in(other, number)
            });
        } else {
            let fraction = self.double_in(block, number);
            let is_int32 = self.ir().above_or_equal(number, Value::NUMBER_TAG as i64);
            self.branch_out(is_int32, fraction, true, |lowering, other| {
                lowering.int32_in(other, number)
            });
        }
        Ok(())
    }

    /// ToNumber of a number, a boolean, undefined or null, none of which runs a script.
    pub(super) fn primitive_to_double(&mut self) -> Built {
        let value = self.operand(0);
        let tag = self.tag(abi::NUMBER_TAG);
        let number_bits = self.ir().and(value, tag);
        let is_not_number = self.ir().is_zero(number_bits);
        let block = self.block();
        let is_int32 = self.ir().above_or_equal(value, Value::NUMBER_TAG as i64);
        let from_int32 = self.int32_in(block, value);
        let from_double = self.double_in(block, value);
        let number = self.ir().select(is_int32, from_int32, from_double);
        self.branch_out(is_not_number, number, true, |lowering, other| {
            // Undefined is NaN; null and `false` are zero and `true` is one, which their lowest bit gives.
            let is_undefined = lowering
                .at_end(other)
                .equal(value, Value::UNDEFINED.to_bits() as i64);
            let bit = lowering.at_end(other).and(value, 1);
            let narrow = lowering.at_end(other).wrap(bit);
            let as_number =
                lowering
                    .at_end(other)
                    .convert(Unary::IntToFloat(Sign::Signed), Type::F64, narrow);
            let nan = lowering.at_end(other).const_f64(f64::NAN);
            lowering.at_end(other).select(is_undefined, nan, as_number)
        });
        Ok(())
    }

    /// ToInt32 of a double. Rounding toward zero is exact for what an i64 holds; the rest is rare and wraps.
    pub(super) fn truncate_double(&mut self) -> Built {
        let number = self.operand(0);
        let wide = self
            .ir()
            .convert(Unary::FloatToInt(Sign::Signed), Type::I64, number);
        let narrow = self.ir().wrap(wide);
        let magnitude = self.ir().unary(Unary::Abs, number);
        let limit = self.graph.function.const_f64(TWO_TO_THE_63);
        let huge = self
            .ir()
            .float_compare(FloatCond::GeOrUnordered, magnitude, limit);
        self.branch_out(huge, narrow, true, |lowering, other| {
            let target = crate::runtime::operations::address::to_int32();
            lowering.call_runtime_in(other, target, &[number], Type::I32, Call::Leaf)
        });
        Ok(())
    }

    /// An F64 that has to be an integer other than minus zero.
    pub(super) fn double_as_int32(&mut self) -> Built {
        let number = self.operand(0);
        let int32 = self
            .ir()
            .convert(Unary::FloatToInt(Sign::Signed), Type::I32, number);
        let back = self.int_to_double(int32);
        let differs = self
            .ir()
            .float_compare(FloatCond::NeOrUnordered, back, number);
        self.exit_if(differs, ExitKind::NotAnInteger);
        let is_zero = self.ir().is_zero(int32);
        self.reject_negative_zero(number, is_zero);
        self.graph.function.replace(self.at, int32);
        Ok(())
    }

    /// The same for an integer of 52 bits, through the trick that puts it in the low bits of a double.
    pub(super) fn double_as_int52(&mut self) -> Built {
        let number = self.operand(0);
        let bias = self.graph.function.const_f64(INTEGER_IN_LOW_BITS);
        let biased = self
            .ir()
            .binary(bun_engine_codegen::ir::Binary::Add, number, bias);
        let bits = self.ir().bitcast(biased);
        let shifted = self.ir().shl(bits, 12);
        let signed = self.ir().xor(shifted, SIGN_BIT);
        let int52 = self.ir().sar(signed, 12);
        let back = self.int_to_double(int52);
        let differs = self
            .ir()
            .float_compare(FloatCond::NeOrUnordered, back, number);
        self.exit_if(differs, ExitKind::Int52Overflow);
        let is_zero = self.ir().is_zero(int52);
        self.reject_negative_zero(number, is_zero);
        self.graph.function.replace(self.at, int52);
        Ok(())
    }

    /// Minus zero is the one double that compares equal to an integer without being it.
    fn reject_negative_zero(&mut self, number: Ssa, is_zero: Ssa) {
        let bits = self.ir().bitcast(number);
        let is_negative = self.ir().less_than(bits, 0);
        let both = self.ir().and(is_zero, is_negative);
        self.exit_if(both, ExitKind::NegativeZero);
    }
}
