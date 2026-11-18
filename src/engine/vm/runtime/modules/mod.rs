//! Module records, namespaces and the loader.

mod environment;
mod loader;

pub use loader::{ModuleHooks, ModuleLoader};

pub fn initialize_module_slot(_: &crate::Cx, _: crate::RealmSlot) -> bool {
    false
}

/// One of those from `ImportModuleNamespace` on.
pub(crate) fn run_microtask(
    _: &mut crate::Cx,
    _: crate::InternalMicrotask,
    _: crate::PromiseStatus,
    _cell: crate::Value,
    _value: crate::Value,
    _context: crate::Value,
) -> crate::JsResult<()> {
    Ok(())
}

#[derive(Default)]
pub(crate) struct State {}

impl crate::services::Service for State {}
