use bun_engine_macros::cell;

use crate::{Cx, FieldArray, Gc, JsObject, JsResult, Value};

#[cell(name = "ProxyObject", cell_type = Proxy)]
pub struct ProxyObject {
    base: JsObject,
    /// The target and the handler.
    pub(self) fields: FieldArray<Value, 2>,
}

impl ProxyObject {
    pub fn target(&self) -> Gc<JsObject> {
        self.fields().at(0).cast()
    }

    pub fn is_revoked(&self) -> bool {
        self.fields().at(1).is_null()
    }

    pub(crate) fn put_index_with_receiver(
        &self,
        _: &mut Cx,
        _receiver: Value,
        _index: u32,
        _value: Value,
        _should_throw: bool,
    ) -> JsResult<bool> {
        Ok(false)
    }
}
