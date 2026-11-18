use crate::{Cx, JsResult, ModuleEnvironment, Value};

impl ModuleEnvironment {
    /// Fills an empty slot, if it is one of those that get their value when they are first read.
    pub(crate) fn create_on_demand(&self, _: &mut Cx, _: u32) -> JsResult<Option<Value>> {
        Ok(None)
    }

    /// The instruction `get_import`.
    pub fn get_import(&self, cx: &mut Cx, slot: u32) -> JsResult<Value> {
        self.get_checked(cx, slot)
    }
}
