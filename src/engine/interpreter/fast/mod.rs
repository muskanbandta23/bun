//! The common cases of the common instructions. A handler calls nothing: what needs a call goes on in another one.

pub(crate) mod allocation;
pub(crate) mod calls;
pub(crate) mod control;
pub(crate) mod elements;
pub(crate) mod frames;
pub(crate) mod iteration;
pub(crate) mod operators;
pub(crate) mod properties;
pub(crate) mod scopes;

use bun_engine_bytecode::Op;
use bun_engine_vm::heap::barrier;

use crate::machine::{cell, handler, next};

handler! {
    /// Ends an instruction that has stored into the cell.
    #[cold]
    #[inline(never)]
    fn barrier_and_next<O: Op>(m, op: O, owner) {
        barrier::slow_path(&cell(owner));
        next!(m, op)
    }
}

/// Runs the write barrier for a value that may be a cell, and goes on.
macro_rules! barrier_and_next {
    ($m:ident, $op:ident: $Op:ty, $owner:expr) => {{
        let owner = $owner;
        if !$m.needs_no_barrier(&owner) {
            $crate::machine::go!($m, $crate::fast::barrier_and_next::<$Op, W>; owner.address())
        }
        $crate::machine::next!($m, $op)
    }};
    ($m:ident, $op:ident: $Op:ty, $owner:expr, $value:expr) => {{
        if $m.is_cell($value) {
            $crate::fast::barrier_and_next!($m, $op: $Op, $owner)
        }
        $crate::machine::next!($m, $op)
    }};
}

pub(crate) use barrier_and_next;
