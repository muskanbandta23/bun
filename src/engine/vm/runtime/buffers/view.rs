use core::cell::Cell;

use bun_engine_macros::cell;

use crate::{JsObject, Value};

#[cell(name = "ArrayBufferView", cell_type = Int8Array..=DataView)]
pub struct JsArrayBufferView {
    base: JsObject,
    vector: Cell<*mut u8>,
    length: Cell<usize>,
}

impl JsArrayBufferView {
    pub fn length(&self) -> usize {
        self.length.get()
    }

    /// `None` if the index is out of bounds or the element is a BigInt.
    pub fn try_get_number(&self, _index: usize) -> Option<Value> {
        None
    }
}
