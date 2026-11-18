//! One stub for each kind of case.

use bun_engine_asm::prelude::*;
use bun_engine_bytecode::Specialization;
use bun_engine_vm::ic::{AccessCase, AccessKind, Key, PrototypeLookup};
use bun_engine_vm::{
    CallFrame, CellType, Executable, GetterSetter, GlobalProxy, JsArrayBufferView, JsFunction,
    Shape, Vm, call,
};

use super::abi::*;
use crate::emit::{Emit, FALSE, TRUE, UNDEFINED, WORD, at, shape_id};
use crate::registers::*;

const HANDLER_COUNT: usize = AccessKind::ALL.len() * 2;

pub(super) fn index(kind: AccessKind, key: Key) -> usize {
    kind as usize * 2 + usize::from(key == Key::Variable)
}

const fn case(field: usize) -> Address {
    at(CASE, field)
}

struct Handler<'a> {
    m: &'a mut MacroAssembler,
    name: Gpr,
    miss: Label,
    write_barrier: Label,
}

pub(crate) fn emit_handlers(m: &mut MacroAssembler, write_barrier: Label) -> Vec<Label> {
    let miss = m.here();
    m.move32(RESULT, 0);
    m.ret();
    let mut labels = vec![miss; HANDLER_COUNT];
    for key in [Key::Fixed, Key::Variable] {
        for kind in AccessKind::ALL.into_iter().filter(|kind| !kind.is_last()) {
            m.align(16, 15);
            labels[index(kind, key)] = m.here();
            let other = m.new_label();
            m.branch32(Cond::Ne, SHAPE, case(AccessCase::OFFSET_OF_SHAPE), other);
            if key == Key::Variable {
                m.branch64(Cond::Ne, NAME, case(AccessCase::OFFSET_OF_KEY), other);
            }
            let name = if key == Key::Fixed { KEY } else { NAME };
            Handler {
                m,
                name,
                miss,
                write_barrier,
            }
            .emit(kind, other);
            m.bind(other);
            m.load64(CASE, case(AccessCase::OFFSET_OF_NEXT));
            m.jump_indirect(case(AccessCase::OFFSET_OF_CODE));
        }
    }
    labels
}

impl Handler<'_> {
    fn emit(&mut self, kind: AccessKind, other: Label) {
        let miss = self.miss;
        match kind {
            AccessKind::LoadOwn => {
                self.load_own(BASE);
                self.m.ret();
            }
            AccessKind::LoadConstant => self.return_constant(),
            AccessKind::Instanceof => {
                let prototype = case(AccessCase::OFFSET_OF_POINTER);
                self.m.branch64(Cond::Ne, VALUE, prototype, other);
                self.return_constant();
            }
            AccessKind::LoadInherited => {
                self.load_inherited();
                self.m.ret();
            }
            AccessKind::CallOwnGetter | AccessKind::CallOwnSetter => {
                self.load_own(BASE);
                self.call_accessor(kind == AccessKind::CallOwnSetter);
            }
            AccessKind::CallInheritedGetter | AccessKind::CallInheritedSetter => {
                self.load_inherited();
                self.call_accessor(kind == AccessKind::CallInheritedSetter);
            }
            AccessKind::CallCustomAccessorGetter
            | AccessKind::CallCustomValueGetter
            | AccessKind::CallCustomAccessorSetter
            | AccessKind::CallCustomValueSetter => self.call_custom(kind),
            AccessKind::TypedArrayLength => {
                self.miss_unless_valid();
                self.m.branch64(Cond::Ne, VALUE, BASE, miss);
                (self.m).load64(RESULT, at(BASE, JsArrayBufferView::OFFSET_OF_LENGTH));
                self.m.branch64(Cond::Above, RESULT, i32::MAX, miss);
                self.m.box_int32(RESULT, RESULT);
                self.m.ret();
            }
            AccessKind::LoadModuleBinding => {
                let namespace = case(AccessCase::OFFSET_OF_POINTER);
                self.m.branch64(Cond::Ne, BASE, namespace, miss);
                (self.m).load64(RESULT, case(AccessCase::OFFSET_OF_SECOND_POINTER));
                self.m.load64(RESULT, at(RESULT, 0));
                self.m.ret();
            }
            AccessKind::LoadGlobalBinding | AccessKind::LoadOwnThroughGlobalProxy => {
                self.m.load64(NAME, at(BASE, GlobalProxy::OFFSET_OF_TARGET));
                self.m.load32(RESULT, case(AccessCase::OFFSET_OF_NEW_SHAPE));
                self.m.branch32(Cond::Ne, RESULT, shape_id(NAME), miss);
                if kind == AccessKind::LoadGlobalBinding {
                    self.m.load64(RESULT, case(AccessCase::OFFSET_OF_POINTER));
                    self.m.load64(RESULT, at(RESULT, 0));
                } else {
                    self.load_own(NAME);
                }
                self.m.ret();
            }
            AccessKind::Replace | AccessKind::Transition => {
                if kind == AccessKind::Transition {
                    self.miss_unless_valid();
                }
                (self.m).load32_signed64(SHAPE, case(AccessCase::OFFSET_OF_BYTE_OFFSET));
                self.m.slot_base(RESULT, BASE, SHAPE);
                (self.m).store64(Address::indexed(RESULT, SHAPE, Scale::One, 0), VALUE);
                if kind == AccessKind::Transition {
                    self.m.load32(RESULT, case(AccessCase::OFFSET_OF_NEW_SHAPE));
                    self.m.store32(shape_id(BASE), RESULT);
                }
                return_from_store(self.m, self.write_barrier);
            }
            AccessKind::HasOwn | AccessKind::HasInherited | AccessKind::HasNot => {
                if kind != AccessKind::HasOwn {
                    self.miss_unless_valid();
                }
                let answer = if kind == AccessKind::HasNot {
                    FALSE
                } else {
                    TRUE
                };
                self.m.move32(RESULT, answer);
                self.m.ret();
            }
            _ => self.m.jump(miss),
        }
    }

    fn load_own(&mut self, object: Gpr) {
        (self.m).load32_signed64(SHAPE, case(AccessCase::OFFSET_OF_BYTE_OFFSET));
        self.m.slot_base(RESULT, object, SHAPE);
        (self.m).load64(RESULT, Address::indexed(RESULT, SHAPE, Scale::One, 0));
    }

    fn load_inherited(&mut self) {
        self.m.load64(CASE, case(AccessCase::OFFSET_OF_LOOKUP));
        (self.m).load32_signed64(SHAPE, at(CASE, PrototypeLookup::OFFSET_OF_BYTE_OFFSET));
        (self.m).load64(CASE, at(CASE, PrototypeLookup::OFFSET_OF_HOLDER));
        self.m.branch_if_zero64(CASE, self.miss);
        self.m.slot_base(RESULT, CASE, SHAPE);
        (self.m).load64(RESULT, Address::indexed(RESULT, SHAPE, Scale::One, 0));
    }

    fn return_constant(&mut self) {
        self.m.load64(RESULT, case(AccessCase::OFFSET_OF_LOOKUP));
        (self.m).load64(RESULT, at(RESULT, PrototypeLookup::OFFSET_OF_CONSTANT));
        self.m.ret();
    }

    fn miss_unless_valid(&mut self) {
        self.m.load64(RESULT, case(AccessCase::OFFSET_OF_LOOKUP));
        let is_valid = at(RESULT, PrototypeLookup::OFFSET_OF_IS_VALID);
        self.m.branch8(Cond::Eq, is_valid, 0, self.miss);
    }

    /// With the `GetterSetter` in `RESULT`.
    fn call_accessor(&mut self, is_setter: bool) {
        let (field, this, count) = if is_setter {
            (GetterSetter::OFFSET_OF_SETTER, BASE, 2)
        } else {
            (GetterSetter::OFFSET_OF_GETTER, VALUE, 1)
        };
        let m = &mut *self.m;
        m.load64(CASE, at(RESULT, field));
        m.branch_if_zero64(CASE, self.miss);
        m.branch_unless_type_in(CASE, CellType::FUNCTIONS, RESULT, self.miss);
        let words = (CallFrame::HEADER_WORDS + count).next_multiple_of(2);
        let room = words * WORD as usize - CallFrame::LINKAGE_SIZE;
        let has_return_address_on_stack = cfg!(target_arch = "x86_64");
        if has_return_address_on_stack {
            m.pop(SHAPE);
        }
        m.sub64(SP, SP, room);
        let header = |field: usize| at(SP, field - CallFrame::LINKAGE_SIZE);
        m.load64(EXECUTABLE, at(CASE, JsFunction::OFFSET_OF_EXECUTABLE));
        let code_block = Executable::offset_of_code_block(Specialization::Call);
        m.load64(RESULT, at(EXECUTABLE, code_block));
        m.store_pair64(header(CallFrame::OFFSET_OF_CODE_BLOCK), RESULT, CASE);
        m.store64(header(CallFrame::OFFSET_OF_ARGUMENT_COUNT), count);
        m.store64(header(CallFrame::OFFSET_OF_THIS), this);
        if is_setter {
            m.store64(header(CallFrame::OFFSET_OF_ARGUMENTS), VALUE);
        }
        if has_return_address_on_stack {
            m.push(SHAPE);
        }
        let entry = Executable::offset_of_entry(Specialization::Call);
        m.jump_indirect(at(EXECUTABLE, entry));
    }

    fn call_custom(&mut self, kind: AccessKind) {
        let (m, holder) = (&mut *self.m, SHAPE);
        if kind == AccessKind::CallCustomAccessorGetter {
            m.branch64(Cond::Ne, VALUE, BASE, self.miss);
        }
        let found = m.new_label();
        m.move64(holder, BASE);
        m.load64(RESULT, case(AccessCase::OFFSET_OF_LOOKUP));
        m.branch_if_zero64(RESULT, found);
        m.load64(holder, at(RESULT, PrototypeLookup::OFFSET_OF_HOLDER));
        m.branch_if_zero64(holder, self.miss);
        m.bind(found);
        m.load32(RESULT, shape_id(holder));
        m.decode_shape(RESULT, RESULT);
        m.load64(RESULT, at(RESULT, Shape::OFFSET_OF_GLOBAL));
        m.branch_if_zero64(RESULT, self.miss);
        m.store64(at(VM, Vm::OFFSET_OF_TOP_CALL_FRAME), FP);
        m.enter_frame();
        let function = ENTRY;
        m.load64(function, case(AccessCase::OFFSET_OF_POINTER));
        match kind {
            AccessKind::CallCustomAccessorGetter => {
                m.move_arguments(&[RESULT.into(), BASE.into(), self.name.into()]);
            }
            AccessKind::CallCustomValueGetter => {
                m.move_arguments(&[RESULT.into(), holder.into(), self.name.into()]);
            }
            _ => m.move_arguments(&[RESULT.into(), BASE.into(), VALUE.into(), self.name.into()]),
        }
        m.call_indirect(function);
        m.leave_frame();
        let (throw, done) = (m.external_address(call::throw_from_frame()), m.new_label());
        m.branch_if_not_zero64(at(VM, Vm::OFFSET_OF_EXCEPTION), throw);
        m.branch_if_not_zero64(RESULT, done);
        m.move32(RESULT, UNDEFINED);
        m.bind(done);
        m.ret();
    }
}

pub(super) fn return_from_store(m: &mut MacroAssembler, write_barrier: Label) {
    m.move32(RESULT, 1);
    m.branch_if_needs_barrier(BASE, SHAPE, write_barrier);
    m.ret();
}
