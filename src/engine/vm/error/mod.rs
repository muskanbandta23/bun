//! Exceptions, error objects and stack traces.

mod error_instance;
pub mod errors;
mod exception;
mod source_appender;
mod stack_trace;
mod throw;

pub use error_instance::{ErrorHooks, ErrorInstance, ErrorInstanceInit, ErrorType};
pub use exception::Exception;
pub use source_appender::SourceAppender;
pub use stack_trace::{StackFrame, StackVisitor, VisitedFrame, capture_stack_trace};
pub use throw::Message;

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
