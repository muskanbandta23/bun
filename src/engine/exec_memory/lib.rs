//! Executable memory: allocation, linking and patching of generated code.

mod code;
mod free_space;
mod instruction_cache;
mod pages;
mod pool;

pub use code::{
    CodeHandle, Observer, OutOfExecutableMemory, Owner, Owners, Priority, link, lock_owners,
    owner_of_running_code, replace_with_jump, set_move_value, set_observer, set_transfer_target,
    transfer_target,
};
pub use instruction_cache::synchronize_instructions;
pub use pool::{Usage, contains, is_available, usage};
