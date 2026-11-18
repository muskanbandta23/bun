//! What text costs beyond a load.

use bun_engine_codegen::ir::Value as Ssa;

use super::Lowering;
use crate::builder::{Built, Refusal};

impl Lowering<'_> {
    pub(super) fn string_length(&mut self) -> Ssa {
        self.int32(0)
    }

    pub(super) fn string_unit_at(&mut self) -> Ssa {
        self.int32(0)
    }

    pub(super) fn resolve_rope(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn string_from_unit(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn concatenate(&mut self) -> Built {
        Err(Refusal::Unsupported)
    }

    pub(super) fn store_element_growing(&mut self, _kind: u32) -> Built {
        Err(Refusal::Unsupported)
    }
}
