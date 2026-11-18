//! x86-64.

pub mod abi;
mod assembler;
mod float;
mod integer;
mod layout;
mod opcode;
pub mod patch;
mod registers;

pub use assembler::{Assembler, FprMem, GprMem, Rm};
pub use float::{FloatFlags, rounding_control};
pub use integer::MacroAssembler;
pub use opcode::*;
pub use registers::*;
