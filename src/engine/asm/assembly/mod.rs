//! The operations that every macro assembler has.
//!
//! The destination comes first. Registers may be the same unless a method says otherwise. No operand is one of the
//! scratch registers, which any method may overwrite, as it may the flags. At most one operand is in memory.
//! The stack pointer may be the base of an address and an operand of `move64`, `add64`, `sub64`, `and64` with a
//! constant, and the left side of `branch64`. Moves between registers, moves of trusted constants, loads and
//! `compute_address` leave the first scratch register of each bank alone.

macro_rules! aliases {
    ($($general:ident($($fixed:expr),+) as $name:ident($($argument:ident: $type:ty),*);)*) => {
        $(#[inline]
        fn $name(&mut self, $($argument: $type),*) {
            self.$general($($fixed,)+ $($argument),*)
        })*
    };
}

macro_rules! for_both {
    ($first:expr, $second:expr; $($general:ident as $one:ident, $two:ident $arguments:tt;)*) => {
        aliases! {
            $($general($first) as $one $arguments;
            $general($second) as $two $arguments;)*
        }
    };
}

macro_rules! for_both_after {
    ($first:expr, $second:expr; $($general:ident($fixed:expr) as $one:ident, $two:ident $arguments:tt;)*) => {
        aliases! {
            $($general($fixed, $first) as $one $arguments;
            $general($fixed, $second) as $two $arguments;)*
        }
    };
}

mod call;
mod float;
mod integer;
mod value;
mod vector;

pub use call::{Argument, CallAssembly};
pub use float::FloatAssembly;
pub use integer::{MacroAssembly, Options, Scratch, ScratchNeeds};
pub use value::ValueAssembly;
pub use vector::{Reuse, VectorAssembly};
