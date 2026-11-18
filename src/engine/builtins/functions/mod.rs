//! `Function`, errors, `eval`, `ShadowRealm` and `console`.

mod error;
mod eval;
pub(crate) mod function;

use bun_engine_vm::{
    CellKind, JsBoundFunction, JsCustomGetterFunction, JsCustomSetterFunction, JsNativeClosure,
    JsRemoteFunction, RealmSlot, RealmWatchpoint,
};

use crate::prelude::*;
use crate::watchpoints::Conditions;

fn function_shape<T: CellKind>(cx: &Cx) -> Gc<Shape> {
    let prototype = cx.function_prototype().into();
    Shape::create(cx, Some(cx.global()), prototype, ShapeSpec::of::<T>())
}

pub(crate) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
    use RealmSlot::*;
    let global = cx.global();
    match slot {
        function_constructor => function::initialize_constructor(cx),
        function_prototype_call => function::initialize_call(cx),
        function_prototype_apply => function::initialize_apply(cx),
        function_prototype_has_instance => function::initialize_has_instance(cx),
        throw_type_error_function | throw_type_error_accessor => function::initialize_thrower(cx),
        bound_function_shape => {
            global.init_bound_function_shape(function_shape::<JsBoundFunction>(cx));
        }
        custom_getter_function_shape => {
            global.init_custom_getter_function_shape(function_shape::<JsCustomGetterFunction>(cx));
        }
        custom_setter_function_shape => {
            global.init_custom_setter_function_shape(function_shape::<JsCustomSetterFunction>(cx));
        }
        native_closure_shape => {
            global.init_native_closure_shape(function_shape::<JsNativeClosure>(cx));
        }
        remote_function_shape => {
            global.init_remote_function_shape(function_shape::<JsRemoteFunction>(cx));
        }
        eval_function => eval::initialize(cx),
        _ => return error::initialize_slot(cx, slot),
    }
    true
}

pub(crate) fn add_conditions(_: &Cx, _: RealmWatchpoint, _: &mut Conditions<'_>) -> bool {
    false
}
