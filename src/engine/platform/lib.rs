//! What the engine asks of the operating system: clocks, the size of the machine, stacks and other threads.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod cgroup;
pub mod clock;
pub mod machine;
mod stack;
pub mod suspend;

pub use stack::StackBounds;
