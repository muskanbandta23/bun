use std::rc::Rc;

use bun_engine_bytecode::{Code, Specialization};

use crate::{BuiltinId, Cx, FunctionExecutable, Gc, JsResult, SourceCode, Thrown, Value, Vm};

pub fn throw_compile_error(cx: &Cx) -> Thrown {
    cx.throw_syntax_error("")
}

pub fn evaluate_script(cx: &mut Cx, _: &SourceCode, _this: Value) -> JsResult<Value> {
    Err(throw_compile_error(cx))
}

pub(super) fn compile_function(
    cx: &Cx,
    _: &FunctionExecutable,
    _: Specialization,
) -> JsResult<Rc<Code>> {
    Err(throw_compile_error(cx))
}

impl FunctionExecutable {
    pub(crate) fn for_builtin(_: &Vm, _: BuiltinId) -> Gc<FunctionExecutable> {
        unreachable!()
    }
}
