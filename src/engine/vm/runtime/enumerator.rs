use bun_engine_macros::cell;

use crate::{JsCell, TypeFlags};

#[cell(cell_type = PropertyNameEnumerator, flags = TypeFlags::SHAPE_IS_IMMORTAL)]
pub struct PropertyNameEnumerator {
    base: JsCell,
}
