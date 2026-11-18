//! The instruction set, the bytecode generator and the serialized form of code.

#![forbid(unsafe_code)]
#![allow(dead_code, unused_imports)]

mod code;
mod constant;
mod function;
mod host;
mod instruction;
mod metadata;
mod operand;
mod positions;
mod scope;
mod table;

pub use bun_engine_syntax::{EvalContext, FunctionKind};
pub use code::*;
pub use constant::*;
pub use function::{FunctionInfo, FunctionRef, Origin, Specialization, Visibility};
pub use host::{BuiltinValue, Host};
pub use instruction::{Flow, Instruction, Op, Opcode, RawInstruction, Shape, op};
pub use metadata::{MetadataKind, MetadataLayout};
pub use operand::*;
pub use positions::{Position, Positions};
pub use scope::{ScopeKind, Slot, SlotFlags, SlotKind, StaticScope};
