use bun_engine_macros::cell;

use crate::JsObject;

#[cell(name = "Promise", cell_type = Promise)]
pub struct JsPromise {
    base: JsObject,
}
