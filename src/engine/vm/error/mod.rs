//! Exceptions, error objects and stack traces.

mod async_stack_trace;
mod error_info;
mod error_instance;
pub mod errors;
mod exception;
mod source_appender;
mod stack_frame;
mod stack_trace;
mod stack_visitor;
mod termination;
mod throw;

use core::cell::Cell;

pub use error_info::{ErrorHooks, ErrorLocation, add_error_info, define_captured_stack};
pub use error_instance::{ErrorInstance, ErrorType};
pub use exception::Exception;
pub use source_appender::SourceAppender;
pub use stack_frame::{StackFrame, WasmNames, stack_trace_text};
pub use stack_trace::{StackTrace, StackTraceRequest, capture_stack_trace};
pub use stack_visitor::{FrameKind, StackVisitor, VisitedFrame, WasmFrame};
pub use termination::{AfterDeferral, DeferTermination, TerminationDeadline};
pub use throw::Message;

use crate::Vm;
use crate::traps::BlockedThread;

#[derive(Default)]
pub(crate) struct State {
    entry_contexts: stack_visitor::EntryContexts,
    termination_deferral: termination::Deferral,
    has_had_deadlines: Cell<bool>,
    pub(crate) blocked_thread: BlockedThread,
}

impl crate::services::Service for State {
    fn will_destroy(&self, vm: &Vm) {
        vm.cancel_termination_deadlines();
    }
}
