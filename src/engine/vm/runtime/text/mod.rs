//! Regular expressions, dates and JSON.

mod regexp;

pub use regexp::{RegExp, RegExpGlobalState};

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
