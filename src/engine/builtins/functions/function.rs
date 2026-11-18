use bun_engine_vm::{JsCallee, JsFunction};

use crate::prelude::*;

static_table!(static PROTOTYPE_PROPERTIES = []);

#[cell(name = "Function", static_table = PROTOTYPE_PROPERTIES)]
struct FunctionPrototype {
    base: InternalFunction,
}

pub(crate) fn initialize_prototype(cx: &Cx) {
    let (global, parent) = (cx.global(), cx.object_prototype().into());
    let spec = ShapeSpec::of::<FunctionPrototype>();
    let shape = Shape::create_dictionary(cx, Some(global), parent, spec);
    let base = InternalFunction::init(cx, shape, call_prototype, None);
    let prototype = cx.allocate::<FunctionPrototype>(FunctionPrototypeInit { base });
    prototype.define_length_and_name(cx, 0, cx.small_strings.empty());
    prototype.flatten_dictionary(cx);
    let spec = ShapeSpec::of::<JsFunction>();
    let shape = || Shape::create(cx, Some(global), prototype.into(), spec);
    global.init_function_prototype(prototype.upcast());
    global.init_function_shape(shape());
    global.init_method_shape(shape());
    global.init_builtin_function_shape(shape());
    global.init_native_function_shape(shape());
    let spec = ShapeSpec::of::<JsCallee>();
    global.init_callee_shape(Shape::create(cx, Some(global), Value::NULL, spec));
}

#[native_abi]
fn call_prototype(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(Value::UNDEFINED)
}
