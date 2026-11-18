//! Throwing and catching.

use bun_engine_asm::Code;
use bun_engine_asm::prelude::*;
use bun_engine_wasm_format::{BlockType, Catch, Flow, HeapType, TagIndex, ValType};

use super::calls::Parameter;
use super::cold::Cold;
use super::control::BlockKind;
use super::edges::Carried;
use super::frame::CALLEE_WORDS_SIZE;
use super::operand::{Operand, bank};
use super::{Compiled, Function};
use crate::abi::{INSTANCE, Operation, RESULT_GPRS, payload_offsets};
use crate::output::CallSite;

/// A position in [`Handlers`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Handler(u32);

impl Handler {
    /// The exception leaves the function.
    pub(super) const CALLER: Self = Self(u32::MAX);
}

#[derive(Clone, Copy)]
enum Handling {
    /// Nothing can be thrown so far.
    Unused,
    Used,
    At(Label),
    /// By one that was made earlier.
    Delegated(Handler),
}

#[derive(Default)]
pub(super) struct Handlers {
    handling: Vec<Handling>,
    call_sites: Vec<(Label, Handler)>,
}

impl Handlers {
    pub(super) fn clear(&mut self) {
        self.handling.clear();
        self.call_sites.clear();
    }

    fn add(&mut self) -> Handler {
        self.handling.push(Handling::Unused);
        Handler(self.handling.len() as u32 - 1)
    }

    fn is_used(&self, handler: Handler) -> bool {
        !matches!(self.handling[handler.0 as usize], Handling::Unused)
    }

    pub(super) fn note_use(&mut self, handler: Handler) {
        if let Some(handling @ Handling::Unused) = self.handling.get_mut(handler.0 as usize) {
            *handling = Handling::Used;
        }
    }

    fn delegate(&mut self, from: Handler, to: Handler) {
        if self.is_used(from) {
            self.note_use(to);
            self.handling[from.0 as usize] = Handling::Delegated(to);
        }
    }

    pub(super) fn call_sites(&mut self, code: &Code) -> Vec<CallSite> {
        for index in 0..self.handling.len() {
            if let Handling::Delegated(to) = self.handling[index]
                && to != Handler::CALLER
            {
                self.handling[index] = self.handling[to.0 as usize];
            }
        }
        let site = |&(end, handler): &(Label, Handler)| match self.handling[handler.0 as usize] {
            Handling::At(label) => Some(CallSite {
                return_offset: code.offset_of(end),
                handler_offset: code.offset_of(label),
                inline_frame: u32::MAX,
            }),
            _ => None,
        };
        self.call_sites.iter().filter_map(site).collect()
    }
}

const EXCEPTION: Gpr = RESULT_GPRS[0];
const TAG: Gpr = RESULT_GPRS[1];
const EXNREF: ValType = HeapType::EXN.non_null().value_type();

impl Function<'_> {
    /// After a call that may throw.
    #[inline]
    pub(super) fn note_call_site(&mut self, handler: Handler) {
        if handler != Handler::CALLER {
            self.handlers.note_use(handler);
            let end = self.masm.mark();
            self.handlers.call_sites.push((end, handler));
        }
    }

    pub(super) fn open_try(&mut self, kind: BlockKind, ty: BlockType) {
        self.settle(self.height() - self.param_count(ty));
        let handler = self.handlers.add();
        self.open(kind, ty, handler);
    }

    fn outer_handler(&self) -> Handler {
        self.blocks[self.blocks.len() - 2].handler
    }

    pub(super) fn end_try_delegating(&mut self, to: Handler, flow: Flow) -> Compiled {
        let handler = self.innermost().handler;
        self.handlers.delegate(handler, to);
        self.close(flow)
    }

    pub(super) fn end_try_without_catch(&mut self, flow: Flow) -> Compiled {
        self.end_try_delegating(self.outer_handler(), flow)
    }

    /// Leaves the exception and its tag in their registers.
    fn enter_handler(&mut self, handler: Handler, base: u32) {
        self.handlers.handling[handler.0 as usize] = Handling::At(self.masm.here());
        self.forget_registers(base);
        self.call_operation(Operation::TakeException, &[]);
        self.load_checked_memory();
        let tag = Address::new(EXCEPTION, self.env.host.exception_tag);
        self.masm.load64(TAG, tag);
    }

    fn branch_unless_tag_is(&mut self, tag: TagIndex) -> Label {
        let unmatched = self.masm.new_label();
        let expected = Address::new(INSTANCE, self.env.layout.tag(tag));
        self.masm.branch64(Cond::Ne, TAG, expected, unmatched);
        unmatched
    }

    fn payload(&self, offset: u32) -> Address {
        Address::new(EXCEPTION, self.env.host.exception_payload + offset as i32)
    }

    pub(super) fn start_catch(&mut self, tag: Option<TagIndex>, flow: Flow) -> Compiled {
        self.leave_arm(flow)?;
        let block = *self.innermost();
        let (is_reached, exception) = match block.kind {
            BlockKind::Catch {
                unmatched,
                exception,
            } => {
                if let Some(label) = unmatched {
                    self.masm.bind(label);
                    self.forget_registers(block.base);
                }
                (unmatched.is_some(), exception)
            }
            _ => {
                let exception = self.frame.exception(self.catch_depth);
                self.catch_depth += 1;
                self.innermost().handler = self.outer_handler();
                let is_reached = self.handlers.is_used(block.handler);
                if is_reached {
                    self.enter_handler(block.handler, block.base);
                    (self.masm).store64(self.frame_address(exception), EXCEPTION);
                }
                (is_reached, exception)
            }
        };
        let tag = tag.filter(|_| is_reached);
        let unmatched = tag.map(|tag| self.branch_unless_tag_is(tag));
        self.innermost().kind = BlockKind::Catch {
            unmatched,
            exception,
        };
        self.is_dead = !is_reached;

        let types = tag.map_or(&[][..], |tag| self.module.tag_params(tag));
        for (&ty, offset) in types.iter().zip(payload_offsets(types)) {
            self.registers.locked = RegisterSet::of(&[EXCEPTION], &[]);
            let reg = self.take(bank(ty.kind()));
            self.load(ty.kind(), reg, self.payload(offset));
            self.push_register(ty, reg);
        }
        self.check_size()
    }

    pub(super) fn end_catch(&mut self, flow: Flow) -> Compiled {
        let block = *self.innermost();
        self.catch_depth -= 1;
        if let BlockKind::Catch {
            unmatched: Some(entry),
            ..
        } = block.kind
        {
            self.throw_unmatched(entry, block.handler);
        }
        self.close(flow)
    }

    fn throw_unmatched(&mut self, entry: Label, handler: Handler) {
        self.handlers.note_use(handler);
        self.cold.push(Cold::ThrowAgain {
            entry,
            exception: EXCEPTION,
            handler,
        });
    }

    pub(super) fn end_try_table(&mut self, first_catch: u32, flow: Flow) -> Compiled {
        let block = *self.innermost();
        let mut flow = flow;
        if self.handlers.is_used(block.handler) {
            self.leave_arm(flow)?;
            flow = Flow::Dead;
            self.enter_handler(block.handler, block.base);
            let mut catches_all = false;
            for index in first_catch as usize..self.catches.len() {
                let catch = self.catches[index];
                let unmatched = catch.tag.map(|tag| self.branch_unless_tag_is(tag));
                self.branch_with_payload(catch)?;
                match unmatched {
                    Some(label) => self.masm.bind(label),
                    None => {
                        catches_all = true;
                        break;
                    }
                }
            }
            if !catches_all {
                let entry = self.masm.new_label();
                self.masm.jump(entry);
                self.throw_unmatched(entry, self.outer_handler());
            }
        }
        self.catches.truncate(first_catch as usize);
        self.close(flow)
    }

    fn branch_with_payload(&mut self, catch: Catch) -> Compiled {
        let index = self.block_index(catch.label + 1);
        if index == 0 && self.blocks[0].target.is_none() {
            self.blocks[0].target = Some(self.target_for_return());
        }
        let block = &self.blocks[index];
        let base = block.base;
        let goes_in_register = self.label_arity(block) == 1 && !block.is_loop();
        let types = catch.tag.map_or(&[][..], |tag| self.module.tag_params(tag));
        let mut carried = Carried::Nothing;
        for ((&ty, offset), position) in types.iter().zip(payload_offsets(types)).zip(base..) {
            if goes_in_register {
                self.registers.locked = RegisterSet::of(&[EXCEPTION, TAG], &[]);
                let reg = self.take(bank(ty.kind()));
                self.load(ty.kind(), reg, self.payload(offset));
                carried = Carried::One(Operand::in_register(ty, reg));
            } else {
                let slot = self.frame_address(self.frame.operand(position));
                self.transfer(ty.kind(), slot, self.payload(offset));
            }
        }
        if catch.passes_exnref {
            if goes_in_register {
                carried = Carried::One(Operand::in_register(EXNREF, EXCEPTION));
            } else {
                let slot = self.frame_address(self.frame.operand(base + types.len() as u32));
                self.masm.store64(slot, EXCEPTION);
            }
        }
        let start = self.steps.len();
        let target = self.late_route(index, carried);
        self.emit_steps_from(start);
        self.masm.jump(target);
        self.check_size()
    }

    /// What a handler in this function reads has to be in the frame.
    fn save_for_handler(&mut self) -> Handler {
        let handler = self.innermost().handler;
        if handler != Handler::CALLER {
            self.write_back_all(self.registers.dirty);
        }
        handler
    }

    pub(super) fn throw_new(&mut self, tag: TagIndex) -> Compiled {
        let handler = self.save_for_handler();
        let types = self.module.tag_params(tag);
        let size = payload_offsets(types).last().expect("the size");
        let size = size.next_multiple_of(16);
        self.lower_stack(size);
        let payload = Address::new(abi::STACK_POINTER, CALLEE_WORDS_SIZE as i32);
        let first = self.stack.len() - types.len();
        for (index, offset) in (first..self.stack.len()).zip(payload_offsets(types)) {
            self.store_operand(payload.plus(offset as i32), self.stack[index]);
        }
        let arguments = [
            Parameter::Immediate(tag.0.into()),
            Parameter::Address(payload),
        ];
        self.call_operation(Operation::Throw, &arguments);
        self.note_call_site(handler);
        self.stack_depth -= size;
        self.check_size()
    }

    pub(super) fn throw_again(&mut self, exception: Operand) {
        let handler = self.save_for_handler();
        self.call_operation(Operation::ThrowRef, &[Parameter::Value(exception)]);
        self.note_call_site(handler);
    }
}
