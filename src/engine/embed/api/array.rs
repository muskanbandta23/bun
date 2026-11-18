use bun_engine_macros::export;
use bun_engine_vm::{
    Cx, ElementKind, Gc, ImmutableArray, JsArray, JsResult, Shape, Value, Vm, is_array,
};

#[export]
fn array_try_create(vm: &Vm, shape: Gc<Shape>, length: u32) -> Option<Gc<JsArray>> {
    JsArray::try_create(vm, shape, length, 0).ok()
}

#[export]
fn array_try_create_dense(vm: &Vm, shape: Gc<Shape>, length: u32) -> Option<Gc<JsArray>> {
    JsArray::try_create_dense(vm, shape, length).ok()
}

#[export]
fn array_with_length(cx: &Cx, length: u32) -> JsResult<Gc<JsArray>> {
    cx.new_array_with_length(ElementKind::Undecided, length)
}

#[export]
fn array_from_values(cx: &Cx, values: &[Value]) -> JsResult<Gc<JsArray>> {
    cx.new_array_from(values)
}

#[export]
fn array_from_values_with_shape(
    cx: &Cx,
    shape: Gc<Shape>,
    values: &[Value],
) -> JsResult<Gc<JsArray>> {
    cx.or_out_of_memory(JsArray::try_from_values_with_shape(cx, shape, values))
}

#[export]
fn array_set_length(
    array: &JsArray,
    cx: &mut Cx,
    length: u32,
    should_throw: bool,
) -> JsResult<bool> {
    array.set_length(cx, length, should_throw)
}

#[export]
fn array_push(array: &JsArray, cx: &mut Cx, value: Value) -> JsResult<()> {
    array.push(cx, value)
}

#[export]
fn array_is_iteration_pristine(array: &JsArray) -> bool {
    array.is_iteration_pristine()
}

#[export]
fn proxy_is_array(cx: &Cx, value: Value) -> JsResult<bool> {
    is_array(cx, value)
}

#[export]
fn immutable_array_try_from_values(vm: &Vm, values: &[Value]) -> Option<Gc<ImmutableArray>> {
    ImmutableArray::try_from_values(vm, values).ok()
}
