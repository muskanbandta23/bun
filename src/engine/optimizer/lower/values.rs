//! The tests and comparisons that specialization leaves to run time.

use bun_engine_asm::abi;
use bun_engine_codegen::ir::{Block, Type, Value as Ssa};
use bun_engine_vm::{CellType, Value};

use super::Lowering;
use super::runtime_calls::Call;
use crate::builder::{Built, Refusal};
use crate::graph::ExitKind;
use crate::runtime::operations::{address, answer};

impl Lowering<'_> {
    pub(super) fn is_type(&mut self, data: u32) -> Result<Ssa, Refusal> {
        let check = self.graph.type_check_at(data);
        let value = self.operand(0);
        self.test_type(value, check.wanted, check.known)
            .ok_or(Refusal::Unsupported)
    }

    pub(super) fn is_cell_of_type(&mut self, data: u32) -> Result<Ssa, Refusal> {
        let value = self.operand(0);
        let mask = self.tag(abi::NOT_CELL_MASK);
        let tag_bits = self.ir().and(value, mask);
        let is_pointer = self.ir().is_zero(tag_bits);
        let is_set = self.ir().is_not_zero(value);
        let is_cell = self.ir().and(is_pointer, is_set);
        let cell_type = self.load_cell_type(value);
        let wanted = CellType(data as u8);
        let matches = self.ir().equal(cell_type, i32::from(wanted.0));
        Ok(self.ir().and(is_cell, matches))
    }

    /// Whether the value is null or undefined, where nothing masquerades as undefined.
    pub(super) fn generic_equal_null(&mut self) -> Ssa {
        let value = self.operand(0);
        let without_undefined = self.ir().and(value, !(Value::UNDEFINED_TAG as i64));
        self.ir().equal(without_undefined, Value::OTHER_TAG as i64)
    }

    /// Two atoms hold the same characters only if they are the same cell.
    pub(super) fn atom_equal(&mut self) -> Ssa {
        let (lhs, rhs) = (self.operand(0), self.operand(1));
        self.ir().equal(lhs, rhs)
    }

    /// Exits where the answer of a run-time function says that an exception is pending.
    pub(super) fn exit_on_exception(&mut self, given: Ssa) {
        let threw = self.ir().equal(given, answer::THREW as i32);
        self.exit_if(threw, ExitKind::Exception);
    }

    /// The same, at the end of a block of its own. `is_empty` is for a function that answers with a value.
    pub(super) fn exit_in(&mut self, block: Block, given: Ssa, kind: ExitKind, is_empty: bool) {
        let threw = if is_empty {
            self.at_end(block).is_zero(given)
        } else {
            self.at_end(block).equal(given, answer::THREW as i32)
        };
        let exit = self.exit(kind);
        self.at_end(block).check(threw, exit);
    }

    pub(super) fn generic_to_boolean(&mut self) -> Built {
        let args = self.with_frame(&[self.operand(0)]);
        let given = self.call_runtime(
            address::generic_to_boolean(),
            &args,
            Type::I32,
            Call::Operation,
        );
        self.graph.function.replace(self.at, given);
        Ok(())
    }

    /// Two strings that are not both atoms. The same cell holds the same characters.
    pub(super) fn string_equal(&mut self) -> Built {
        let (lhs, rhs) = (self.operand(0), self.operand(1));
        let identical = self.ir().equal(lhs, rhs);
        let frame = self.frame_pointer();
        self.diamond(
            identical,
            false,
            |lowering, block| lowering.at_end(block).const32(1),
            |lowering, block| {
                let args = [frame, lhs, rhs];
                let given = lowering.call_runtime_in(
                    block,
                    address::string_equal(),
                    &args,
                    Type::I32,
                    Call::Operation,
                );
                lowering.exit_in(block, given, ExitKind::Exception, false);
                given
            },
        );
        Ok(())
    }

    pub(super) fn primitive_compare(&mut self, relation: u32) -> Built {
        let which = self.int32(relation as i32);
        let args = self.with_frame(&[which, self.operand(0), self.operand(1)]);
        self.answer_of(address::primitive_compare(), &args)
    }

    pub(super) fn quiet_loose_equal(&mut self) -> Built {
        let args = self.with_frame(&[self.operand(0), self.operand(1)]);
        self.answer_of(address::quiet_loose_equal(), &args)
    }

    pub(super) fn generic_strict_equal(&mut self) -> Built {
        let args = self.with_frame(&[self.operand(0), self.operand(1)]);
        self.answer_of(address::generic_strict_equal(), &args)
    }

    pub(super) fn double_remainder(&mut self) -> Built {
        let args = [self.operand(0), self.operand(1)];
        let given = self.call_runtime(address::double_remainder(), &args, Type::F64, Call::Leaf);
        self.graph.function.replace(self.at, given);
        Ok(())
    }

    /// The arguments of a run-time function that wants the frame first.
    fn with_frame(&mut self, rest: &[Ssa]) -> smallvec::SmallVec<[Ssa; 4]> {
        let frame = self.frame_pointer();
        core::iter::once(frame)
            .chain(rest.iter().copied())
            .collect()
    }

    /// A call that gives a boolean, where a third answer says that an exception is pending.
    fn answer_of(&mut self, target: usize, args: &[Ssa]) -> Built {
        let given = self.call_runtime(target, args, Type::I32, Call::Operation);
        self.exit_on_exception(given);
        self.graph.function.replace(self.at, given);
        Ok(())
    }
}
