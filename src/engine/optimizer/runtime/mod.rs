//! What runs on the thread of the `Vm`: compiling, installing, leaving and walking optimized frames.

pub(crate) mod cells;
mod code;
mod exits;
mod machine;
mod plan;
#[cfg_attr(target_arch = "aarch64", path = "stubs_aarch64.rs")]
#[cfg_attr(target_arch = "x86_64", path = "stubs_x86_64.rs")]
mod stubs;
#[cfg(test)]
mod tests;
mod values;

pub use plan::{jettison, tier_up};

/// What an exit jumps to. It is reached through [`crate::lower::output::symbol::EXIT`].
pub(crate) fn exit_thunk() -> usize {
    stubs::exit as *const () as usize
}

/// What `Kind::TierUpAtReturn` calls once the counter of the code block has run out. It takes the `Vm` and the frame.
pub(crate) fn tier_up_at_return() -> usize {
    plan::tier_up_at_return as *const () as usize
}
