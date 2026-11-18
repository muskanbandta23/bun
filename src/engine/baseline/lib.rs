//! The baseline compiler: a template of machine code for each instruction, and the stubs of the inline caches.

mod compiler;
mod emit;
pub mod ic;
mod profiles;
pub mod registers;
mod runtime;
mod thunks;

pub use ic::access_handler;
pub use runtime::{catch_address, entry, install, resume_entry, return_point};
