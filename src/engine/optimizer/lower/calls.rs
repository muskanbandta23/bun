//! Calls of other functions, which build the callee's frame at the bottom of this one.

use super::Lowering;
use crate::builder::{Built, Refusal};
use crate::graph::Kind;

impl Lowering<'_> {
    pub(super) fn call(&mut self, _kind: Kind, _arguments: u32) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn tail_call(&mut self, _kind: Kind, _arguments: u32) -> Built {
        Err(Refusal::Unsupported)
    }
}
