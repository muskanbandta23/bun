use core::cell::Cell;

use bun_engine_macros::cell;

use crate::{
    ErrorInstance, Gc, JsCell, JsString, StackFrame, TracedVec, TypeFlags, Value, Vm,
    capture_stack_trace, options,
};

/// A value that has been thrown, and from where.
#[cell(cell_type = Exception, flags = TypeFlags::SHAPE_IS_IMMORTAL, export = "engine_class_Exception")]
pub struct Exception {
    base: JsCell,
    value: Value,
    pub stack: TracedVec<StackFrame>,
    did_notify_debugger: Cell<bool>,
}

impl Exception {
    pub const OFFSET_OF_VALUE: usize = core::mem::offset_of!(Exception, value);

    pub fn create_without_stack(vm: &Vm, value: Value) -> Gc<Exception> {
        vm.allocate::<Exception>(ExceptionInit {
            base: vm.shapes.exception(),
            value,
            did_notify_debugger: Cell::new(false),
        })
    }

    pub fn create(vm: &Vm, value: Value) -> Gc<Exception> {
        let exception = Self::create_without_stack(vm, value);
        let limit = if value.is::<ErrorInstance>() {
            1
        } else {
            options().exception_stack_trace_limit
        };
        let frames = capture_stack_trace(vm, 0, limit as usize, None);
        exception.stack().extend_from_slice(&frames);
        exception
    }

    pub(crate) fn create_termination(vm: &Vm) -> Gc<Exception> {
        let value = JsString::from_literal(vm, "JavaScript execution terminated.");
        Self::create_without_stack(vm, value.into())
    }

    #[inline(always)]
    pub fn value(&self) -> Value {
        self.value
    }

    pub fn did_notify_debugger(&self) -> &Cell<bool> {
        &self.did_notify_debugger
    }
}
