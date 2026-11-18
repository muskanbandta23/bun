use bun_engine_macros::cell;

use crate::JsCell;

pub struct ModuleHooks {}

#[cell(cell_type = ModuleLoader)]
pub struct ModuleLoader {
    base: JsCell,
}
