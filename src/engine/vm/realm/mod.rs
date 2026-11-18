//! A global object and everything else that a realm has one of.

mod bad_time;
mod bindings;
mod global_object;
mod global_proxy;
mod hooks;
mod slots;
mod watchpoints;

pub use bindings::{GlobalBinding, GlobalBindings};
pub use global_object::{GlobalObject, GlobalObjectInit, InspectorState};
pub use global_proxy::GlobalProxy;
pub use hooks::{PromiseRejectionOperation, RealmHooks, ScriptExecutionStatus};
pub use slots::RealmSlot;
pub use watchpoints::RealmWatchpoint;
