//! Making sure that a value is what the code was compiled for.

use bun_engine_asm::{Sign, Size};
use bun_engine_codegen::ir::{Access, AccessFlags, Type, Value as Ssa};
use bun_engine_vm::{JsCell, JsFunction, ShapeId};

use super::Lowering;
use crate::builder::{Built, Refusal};
use crate::graph::{ElementCheck, ExitKind, ShapeSet, heaps};

impl Lowering<'_> {
    pub(super) fn check_type(&mut self, data: u32) -> Built {
        let check = self.graph.type_check_at(data);
        let value = self.operand(0);
        let Some(holds) = self.test_type(value, check.wanted, check.known) else {
            return Err(Refusal::Unsupported);
        };
        let fails = self.ir().xor(holds, 1);
        self.exit_if(fails, check.failure);
        self.graph.function.replace(self.at, value);
        Ok(())
    }

    pub(super) fn check_shape(&mut self, data: u32) -> Built {
        let cell = self.operand(0);
        let set = ShapeSet(data);
        if set == ShapeSet::ANY {
            self.graph.function.replace(self.at, cell);
            return Ok(());
        }
        let members: smallvec::SmallVec<[ShapeId; 8]> = self
            .graph
            .shapes
            .members(set)
            .iter()
            .map(|&shape| self.graph.shapes[shape].id)
            .collect();
        let id = self.load_shape_id_of(cell);
        let mut differs = self.int32(1);
        for shape in members {
            let is_it = self.ir().equal(id, shape.bits() as i32);
            let not_it = self.ir().xor(is_it, 1);
            differs = self.ir().and(differs, not_it);
        }
        self.exit_if(differs, ExitKind::BadShape);
        self.graph.function.replace(self.at, cell);
        Ok(())
    }

    pub(super) fn check_elements(&mut self, data: u32) -> Built {
        let cell = self.operand(0);
        let fails = match ElementCheck::from_data(data) {
            ElementCheck::Mode { mask, expected } => {
                let access = Access::new(heaps::SHAPE, AccessFlags::ALWAYS_READABLE);
                let mode = self.ir().load_narrow(
                    Size::S8,
                    Sign::Unsigned,
                    cell,
                    JsCell::OFFSET_OF_INDEXING_MODE as i32,
                    access,
                );
                let wanted = self.ir().and(mode, i32::from(mask));
                self.ir().not_equal(wanted, i32::from(expected))
            }
            ElementCheck::Type(cell_type) => {
                let seen = self.load_cell_type(cell);
                self.ir().not_equal(seen, i32::from(cell_type.0))
            }
        };
        self.exit_if(fails, ExitKind::BadElements);
        self.graph.function.replace(self.at, cell);
        Ok(())
    }

    pub(super) fn check_identical(&mut self) -> Built {
        let (value, expected) = (self.operand(0), self.operand(1));
        let differs = self.ir().not_equal(value, expected);
        self.exit_if(differs, ExitKind::BadIdentity);
        self.graph.function.replace(self.at, value);
        Ok(())
    }

    pub(super) fn check_executable(&mut self) -> Built {
        let (function, expected) = (self.operand(0), self.operand(1));
        let access = Access::new(heaps::FUNCTION_TARGET, AccessFlags::ALWAYS_READABLE);
        let executable = self.ir().load(
            Type::I64,
            function,
            JsFunction::OFFSET_OF_EXECUTABLE as i32,
            access,
        );
        let differs = self.ir().not_equal(executable, expected);
        self.exit_if(differs, ExitKind::BadExecutable);
        self.graph.function.replace(self.at, function);
        Ok(())
    }

    /// The shape of a cell, which a check and `Kind::LoadShapeId` both read.
    pub(super) fn load_shape_id_of(&mut self, cell: Ssa) -> Ssa {
        let access = Access::new(heaps::SHAPE, AccessFlags::ALWAYS_READABLE);
        self.ir()
            .load(Type::I32, cell, JsCell::OFFSET_OF_SHAPE_ID as i32, access)
    }
}
