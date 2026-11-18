//! What the built-ins that are written in JavaScript call.

use bun_engine_bytecode::Visibility;
use bun_engine_text::StringRef;
use bun_engine_vm::call::native_not_a_constructor;
use bun_engine_vm::{
    Element, JsArray, JsFunction, JsTypedArray, NativeFunction, RealmSlot, errors,
    with_element_type,
};

use super::constructors::{can_skip_iteration, from_pristine_array, from_typed_array};
use super::copy::default_shape;
use super::prototype::valid_receiver;
use super::species::{construct_species, species_constructor};
use crate::prelude::*;

macro_rules! private_functions {
    ($($name:ident, $intrinsic:ident;)*) => {
        pub(super) fn initialize_slot(cx: &Cx, slot: RealmSlot) -> bool {
            match slot {
                $(RealmSlot::${concat(private_, $name)} => {
                    cx.global().${concat(init_private_, $name)}(create(cx, $name, Intrinsic::$intrinsic));
                })*
                _ => return false,
            }
            true
        }
    };
}

private_functions! {
    is_typed_array_view, IsTypedArrayView;
    is_detached, None;
    typed_array_length, None;
    typed_array_from_fast, None;
    typed_array_species_create, None;
}

fn create(cx: &Cx, function: NativeFunction, intrinsic: Intrinsic) -> Gc<JsFunction> {
    let (name, constructor) = (StringRef::empty(), native_not_a_constructor);
    let visibility = Visibility::Private;
    JsFunction::native_with(cx, 0, &name, function, constructor, intrinsic, visibility)
}

#[native_abi]
fn is_typed_array_view(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.argument(0).is::<JsTypedArray>().into())
}

#[native_abi]
fn is_detached(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let array = frame.argument(0).downcast::<JsTypedArray>();
    Ok(array.is_some_and(|it| it.is_detached()).into())
}

/// Throws unless it is a typed array that is within bounds.
#[native_abi]
fn typed_array_length(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(valid_receiver(cx, frame.argument(0))?.1.into())
}

fn from_fast<T: Element>(cx: &mut Cx, items: Value) -> JsResult<Value> {
    if let Some(array) = items.downcast::<JsArray>()
        && can_skip_iteration(&array)
    {
        return from_pristine_array::<T>(cx, default_shape::<T>(cx), &array).map(Into::into);
    }
    if let Some(source) = items.downcast::<JsTypedArray>()
        && source.is_iteration_pristine(cx)
    {
        let length = source.validate(cx)?;
        if source.element_type().is_bigint() == T::TYPE.is_bigint() {
            return from_typed_array::<T>(cx, default_shape::<T>(cx), &source, length)
                .map(Into::into);
        }
        if length != 0 {
            return Err(cx.throw_type_error(errors::CONTENT_TYPES_DIFFER));
        }
    }
    Ok(Value::UNDEFINED)
}

/// `from` without a mapping function, or undefined if that takes the general way.
#[native_abi]
fn typed_array_from_fast(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    match cx.global().element_type_constructed_by(frame.argument(0)) {
        Some(of) => with_element_type!(of, T => from_fast::<T>(cx, frame.argument(1))),
        None => Ok(Value::UNDEFINED),
    }
}

/// With a length.
#[native_abi]
fn typed_array_species_create(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let exemplar = frame.argument(0).cast::<JsTypedArray>();
    let length = frame.argument(1).to_length(cx)? as usize;
    match species_constructor(cx, &exemplar)? {
        Some(species) => construct_species(cx, &exemplar, species, &[length.into()], length),
        None => {
            let shape = cx.global().view_shape(cx, exemplar.cell_type(), false);
            JsTypedArray::create(cx, shape, length)
        }
    }
    .map(Into::into)
}
