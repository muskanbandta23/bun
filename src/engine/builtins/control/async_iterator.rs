//! %AsyncIteratorPrototype% and %AsyncFromSyncIteratorPrototype%.

use bun_engine_text::StringRef;
use bun_engine_vm::{
    JsAsyncFromSyncIterator, JsFunction, JsFunctionWithFields, JsPromise, RealmSlot,
};

use crate::prelude::*;

static_table!(static PROPERTIES = [
    StaticProperty::realm_slot(symbols::asyncIterator, RealmSlot::async_iterator_prototype_async_iterator),
    StaticProperty::function(symbols::asyncDispose, async_dispose, 0),
]);

define_object!(AsyncIteratorPrototype, "AsyncIterator", PROPERTIES);

static_table!(static FROM_SYNC_PROPERTIES = [
    StaticProperty::function(names::next, next, 1),
    StaticProperty::function(names::r#return, r#return, 1),
    StaticProperty::function(names::throw, throw, 1),
]);

define_object!(
    AsyncFromSyncIteratorPrototype,
    "AsyncFromSyncIterator",
    FROM_SYNC_PROPERTIES
);

pub(super) fn initialize(cx: &Cx) {
    let prototype = AsyncIteratorPrototype::create(cx, cx.object_prototype().into());
    cx.global().init_async_iterator_prototype(prototype);
}

pub(super) fn initialize_async_iterator(cx: &Cx) {
    let (name, intrinsic) = (
        StringRef::from_literal("[Symbol.asyncIterator]"),
        Intrinsic::AsyncIterator,
    );
    let function = JsFunction::native_with_intrinsic(cx, 0, &name, async_iterator, intrinsic);
    (cx.global()).init_async_iterator_prototype_async_iterator(function);
}

pub(super) fn initialize_from_sync(cx: &Cx) {
    let prototype = AsyncFromSyncIteratorPrototype::create(cx, Value::NULL);
    let shape = super::instance_shape::<JsAsyncFromSyncIterator>(cx, prototype.into());
    cx.global().init_async_from_sync_iterator_shape(shape);
}

#[native_abi]
fn async_iterator(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.this_value())
}

#[native_abi]
fn async_dispose(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    call_return(cx, frame.this_value())
        .or_else(|thrown| JsPromise::rejected_with_caught(cx, thrown))
        .map(Into::into)
}

fn call_return(cx: &mut Cx, iterator: Value) -> JsResult<Gc<JsPromise>> {
    let method = iterator.get(cx, names::r#return)?;
    if method.is_undefined_or_null() {
        return Ok(JsPromise::fulfilled(cx, Value::UNDEFINED));
    }
    let result = cx.call(method, iterator, &[])?;
    let result = JsPromise::resolved(cx, result)?;
    let unwrap = JsFunctionWithFields::new(cx, 1, return_undefined, [Value::UNDEFINED; 2]);
    let promise = JsPromise::new(cx);
    result.perform_then(cx, unwrap.into(), Value::UNDEFINED, promise.into());
    Ok(promise)
}

#[native_abi]
fn return_undefined(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(Value::UNDEFINED)
}

fn this_and_argument(frame: &CallFrame) -> (Gc<JsAsyncFromSyncIterator>, Option<Value>) {
    let argument = frame.arguments().first().copied();
    (frame.this_value().cast(), argument)
}

#[native_abi]
fn next(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (iterator, argument) = this_and_argument(frame);
    iterator.next(cx, argument).map(Into::into)
}

#[native_abi]
fn r#return(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (iterator, argument) = this_and_argument(frame);
    iterator.r#return(cx, argument).map(Into::into)
}

#[native_abi]
fn throw(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (iterator, argument) = this_and_argument(frame);
    iterator.throw(cx, argument).map(Into::into)
}
