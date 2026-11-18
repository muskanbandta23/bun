//! What text costs beyond a load, and the element store that grows an array.

use bun_engine_asm::{Sign, Size};
use bun_engine_codegen::ir::{Access, AccessFlags, Type, Value as Ssa};
use bun_engine_text::StringImpl;
use bun_engine_vm::{JsRopeString, JsString, SmallStrings, Vm};

use super::Lowering;
use super::runtime_calls::Call;
use crate::builder::Built;
use crate::graph::{ExitKind, heaps};
use crate::runtime::operations::address;

/// The units that `SmallStrings` has a string for from the start.
const SINGLE_CHARACTERS: u32 = 1 << 8;
const _: () = assert!(bun_engine_text::STATIC_ATOM_COUNT >= SINGLE_CHARACTERS as usize);

impl Lowering<'_> {
    fn fiber_of(&mut self, text: Ssa) -> Ssa {
        let access = Access::new(heaps::STRING_FIBER, AccessFlags::ALWAYS_READABLE);
        self.ir()
            .load(Type::I64, text, JsString::OFFSET_OF_FIBER as i32, access)
    }

    /// A string that is still a tree keeps its length itself; one that is in one piece has it in its characters.
    pub(super) fn string_length(&mut self) -> Built {
        let text = self.operand(0);
        let fiber = self.fiber_of(text);
        let is_rope = self.ir().and(fiber, JsString::IS_ROPE as i64);
        let access = Access::new(heaps::STRING_FIBER, AccessFlags::ALWAYS_READABLE);
        let rope = self.ir().is_not_zero(is_rope);
        self.diamond(
            rope,
            true,
            |lowering, block| {
                let offset = JsRopeString::OFFSET_OF_LENGTH as i32;
                lowering.at_end(block).load(Type::I32, text, offset, access)
            },
            |lowering, block| {
                let offset = StringImpl::OFFSET_OF_LENGTH as i32;
                lowering
                    .at_end(block)
                    .load(Type::I32, fiber, offset, access)
            },
        );
        Ok(())
    }

    /// Of a string that is in one piece, at an index that is in bounds.
    pub(super) fn string_unit_at(&mut self) -> Built {
        let (text, index) = (self.operand(0), self.operand(1));
        let fiber = self.fiber_of(text);
        let header = Access::new(heaps::STRING_FIBER, AccessFlags::ALWAYS_READABLE);
        let characters =
            self.ir()
                .load(Type::I64, fiber, StringImpl::OFFSET_OF_DATA as i32, header);
        let flags = self
            .ir()
            .load(Type::I32, fiber, StringImpl::OFFSET_OF_FLAGS as i32, header);
        let wide = self.ir().zero_extend(index);
        let units = Access::new(heaps::STRING_FIBER, AccessFlags::empty());
        let narrow = self.ir().add(characters, wide);
        let latin1 = self
            .ir()
            .load_narrow(Size::S8, Sign::Unsigned, narrow, 0, units);
        let is_latin1 = self.ir().and(flags, StringImpl::IS_LATIN1 as i32);
        let is_wide = self.ir().is_zero(is_latin1);
        self.branch_out(is_wide, latin1, false, |lowering, block| {
            let offset = lowering.at_end(block).shl(wide, 1);
            let address = lowering.at_end(block).add(characters, offset);
            lowering
                .at_end(block)
                .load_narrow(Size::S16, Sign::Unsigned, address, 0, units)
        });
        Ok(())
    }

    /// Puts the pieces of a string together. One that is already in one piece is given back as it is.
    pub(super) fn resolve_rope(&mut self) -> Built {
        let text = self.operand(0);
        let fiber = self.fiber_of(text);
        let is_rope = self.ir().and(fiber, JsString::IS_ROPE as i64);
        let rope = self.ir().is_not_zero(is_rope);
        let frame = self.frame_pointer();
        self.branch_out(rope, text, true, |lowering, block| {
            let target = address::resolve_rope();
            let args = [frame, text];
            let made = lowering.call_runtime_in(block, target, &args, Type::I64, Call::Operation);
            lowering.exit_in(block, made, ExitKind::Exception, true);
            made
        });
        Ok(())
    }

    /// The string of one code unit, which the shared table usually holds.
    pub(super) fn string_from_unit(&mut self) -> Built {
        let unit = self.operand(0);
        let vm = self.vm();
        let table =
            (Vm::OFFSET_OF_SMALL_STRINGS + SmallStrings::OFFSET_OF_SINGLE_CHARACTERS) as i32;
        let inside = self.ir().and(unit, SINGLE_CHARACTERS as i32 - 1);
        let offset = self.ir().shl(inside, 3);
        let wide = self.ir().zero_extend(offset);
        let place = self.ir().add(vm, wide);
        let access = Access::new(heaps::OTHER_FIELDS, AccessFlags::ALWAYS_READABLE);
        let cached = self.ir().load(Type::I64, place, table, access);
        let is_outside = self.ir().above_or_equal(unit, SINGLE_CHARACTERS as i32);
        let empty = self.ir().const64(0);
        let usable = self.ir().select(is_outside, empty, cached);
        let missing = self.ir().is_zero(usable);
        self.branch_out(missing, usable, true, |lowering, block| {
            let target = address::string_from_unit();
            let args = [vm, unit];
            lowering.call_runtime_in(block, target, &args, Type::I64, Call::Operation)
        });
        Ok(())
    }

    /// Two or three strings, one after the other.
    pub(super) fn concatenate(&mut self) -> Built {
        let operands = self.operands();
        let frame = self.frame_pointer();
        let third = match operands.len() {
            2 => self.word(0),
            _ => operands[2],
        };
        let args = [frame, operands[0], operands[1], third];
        let made = self.call_runtime(address::concatenate(), &args, Type::I64, Call::Operation);
        let threw = self.ir().is_zero(made);
        self.exit_if(threw, ExitKind::Exception);
        self.graph.function.replace(self.at, made);
        Ok(())
    }

    /// A store at an index that the elements do not reach.
    pub(super) fn store_element_growing(&mut self, kind: u32) -> Built {
        let operands = self.operands();
        let (array, index, stored) = (operands[0], operands[2], operands[3]);
        let frame = self.frame_pointer();
        let stored = if self.graph.function.type_of(stored) == Type::F64 {
            self.ir().bitcast(stored)
        } else {
            stored
        };
        let which = self.int32(kind as i32);
        let args = [frame, array, index, stored, which];
        let answer = self.call_runtime(
            address::store_element_growing(),
            &args,
            Type::I32,
            Call::Operation,
        );
        self.exit_on_exception(answer);
        self.graph.function.remove(self.at);
        Ok(())
    }
}
