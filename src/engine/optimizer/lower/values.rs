//! The tests and comparisons that specialization leaves to run time.

use bun_engine_asm::abi;
use bun_engine_codegen::ir::Value as Ssa;
use bun_engine_vm::{CellType, Value};

use super::Lowering;
use crate::builder::{Built, Refusal};

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

    /// Two atoms are equal only if they are the same cell.
    pub(super) fn atom_equal(&mut self) -> Ssa {
        let (lhs, rhs) = (self.operand(0), self.operand(1));
        self.ir().equal(lhs, rhs)
    }

    pub(super) fn generic_to_boolean(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn string_equal(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn primitive_compare(&mut self, _relation: u32) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn quiet_loose_equal(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn generic_strict_equal(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn double_remainder(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }
}
