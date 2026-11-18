//! Array buffers and the views on them.

mod view;

pub use view::JsArrayBufferView;

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
