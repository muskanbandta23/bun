use bun_engine_vm::ErrorInstance;

use crate::prelude::*;

pub(crate) fn initialize(cx: &Cx) {
    let (global, spec) = (cx.global(), ShapeSpec::of::<ErrorInstance>());
    let shape = || Shape::create(cx, Some(global), cx.object_prototype().into(), spec);
    global.init_error_shape(shape());
    global.init_range_error_shape(shape());
    global.init_reference_error_shape(shape());
    global.init_syntax_error_shape(shape());
    global.init_type_error_shape(shape());
}
