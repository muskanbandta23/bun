//! Module records, namespaces and the loader.

mod environment;
mod loader;

pub use loader::{ModuleHooks, ModuleLoader};

pub fn initialize_module_slot(_: &crate::Cx, _: crate::RealmSlot) -> bool {
    false
}

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
