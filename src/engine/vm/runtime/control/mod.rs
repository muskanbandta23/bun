//! Promises, microtasks, generators and async functions.

mod microtask;
mod promise;

pub use microtask::{MicrotaskQueue, MicrotaskQueueRef};
pub use promise::JsPromise;

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
