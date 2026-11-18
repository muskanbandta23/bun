//! Caches of property lookups that all code shares.

#[derive(Default)]
pub(crate) struct State {}

impl State {
    pub(crate) fn did_wrap_epoch(&self) {}
}

impl crate::services::Service for State {}
