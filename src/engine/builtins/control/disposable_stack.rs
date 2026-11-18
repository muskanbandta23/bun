//! `DisposableStack` and `AsyncDisposableStack`.

use bun_engine_vm::{
    CellKind, ErrorType, Extends, FieldArray, GlobalObject, JsArray, JsFunction,
    JsFunctionWithFields, JsObjectInit, RealmSlot, Vm, get_dispose_method, new_suppressed_error,
};

use super::instance_shape;
use crate::js;
use crate::prelude::*;

#[cell(cell_type = AsyncDisposableStack..=DisposableStack)]
struct Stack {
    base: JsObject,
    /// An array that has each resource followed by its method, or null once they are disposed of.
    pub(self) fields: FieldArray<Value, 1>,
}

#[cell(name = "DisposableStack", cell_type = DisposableStack)]
struct JsDisposableStack {
    base: Stack,
}

#[cell(name = "AsyncDisposableStack", cell_type = AsyncDisposableStack)]
struct JsAsyncDisposableStack {
    base: Stack,
}

impl Stack {
    fn resources(&self) -> Option<Gc<JsArray>> {
        self.fields().at(0).downcast()
    }

    fn mark_disposed(&self) {
        self.fields().set(0, Value::NULL);
    }
}

trait Kind: CellKind + Extends<Stack> {
    const IS_ASYNC: bool;
    const CALLBACK: &str;

    fn base_shape(global: &GlobalObject, vm: &Vm) -> Gc<Shape>;

    fn init(base: StackInit) -> Self::Init;

    fn create(cx: &Cx, shape: Gc<Shape>, resources: Gc<JsArray>) -> Value {
        let base = StackInit {
            base: JsObjectInit::new(cx, shape),
            fields: [resources.into()],
        };
        cx.allocate::<Self>(Self::init(base)).into()
    }
}

impl Kind for JsDisposableStack {
    const IS_ASYNC: bool = false;
    const CALLBACK: &str = "onDispose";

    fn base_shape(global: &GlobalObject, vm: &Vm) -> Gc<Shape> {
        global.disposable_stack_shape(vm)
    }

    fn init(base: StackInit) -> JsDisposableStackInit {
        JsDisposableStackInit { base }
    }
}

impl Kind for JsAsyncDisposableStack {
    const IS_ASYNC: bool = true;
    const CALLBACK: &str = "onAsyncDispose";

    fn base_shape(global: &GlobalObject, vm: &Vm) -> Gc<Shape> {
        global.async_disposable_stack_shape(vm)
    }

    fn init(base: StackInit) -> JsAsyncDisposableStackInit {
        JsAsyncDisposableStackInit { base }
    }
}

static_table!(static PROPERTIES = [
    StaticProperty::function(names::adopt, adopt::<JsDisposableStack>, 2),
    StaticProperty::function(names::defer, defer::<JsDisposableStack>, 1),
    StaticProperty::realm_slot(names::dispose, RealmSlot::disposable_stack_prototype_dispose),
    StaticProperty::getter(names::disposed, disposed::<JsDisposableStack>),
    StaticProperty::function(names::r#use, r#use::<JsDisposableStack>, 1),
    StaticProperty::function(names::r#move, r#move::<JsDisposableStack>, 0),
    StaticProperty::realm_slot(symbols::dispose, RealmSlot::disposable_stack_prototype_dispose),
    StaticProperty::string(symbols::toStringTag, names::DisposableStack),
]);

static_table!(static ASYNC_PROPERTIES = [
    StaticProperty::function(names::adopt, adopt::<JsAsyncDisposableStack>, 2),
    StaticProperty::function(names::defer, defer::<JsAsyncDisposableStack>, 1),
    StaticProperty::realm_slot(names::disposeAsync, RealmSlot::async_disposable_stack_prototype_dispose),
    StaticProperty::getter(names::disposed, disposed::<JsAsyncDisposableStack>),
    StaticProperty::function(names::r#move, r#move::<JsAsyncDisposableStack>, 0),
    StaticProperty::function(names::r#use, r#use::<JsAsyncDisposableStack>, 1),
    StaticProperty::realm_slot(symbols::asyncDispose, RealmSlot::async_disposable_stack_prototype_dispose),
    StaticProperty::string(symbols::toStringTag, names::AsyncDisposableStack),
]);

static_table!(static NO_PROPERTIES = []);

define_object!(Prototype, "DisposableStack", PROPERTIES);
define_object!(AsyncPrototype, "AsyncDisposableStack", ASYNC_PROPERTIES);
define_constructor!(
    Constructor,
    NO_PROPERTIES,
    call::<JsDisposableStack>,
    construct::<JsDisposableStack>
);
define_constructor!(
    AsyncConstructor,
    NO_PROPERTIES,
    call::<JsAsyncDisposableStack>,
    construct::<JsAsyncDisposableStack>
);

pub(super) fn initialize(cx: &Cx) {
    let (global, name) = (cx.global(), names::DisposableStack);
    let prototype = Prototype::create(cx, cx.object_prototype().into());
    let constructor = Constructor::create(cx, cx.function_prototype(), 0, name, &prototype);
    global.init_disposable_stack_shape(instance_shape::<JsDisposableStack>(cx, prototype.into()));
    global.init_disposable_stack_constructor(constructor);
}

pub(super) fn initialize_async(cx: &Cx) {
    let (global, name) = (cx.global(), names::AsyncDisposableStack);
    let prototype = AsyncPrototype::create(cx, cx.object_prototype().into());
    let constructor = AsyncConstructor::create(cx, cx.function_prototype(), 0, name, &prototype);
    let shape = instance_shape::<JsAsyncDisposableStack>(cx, prototype.into());
    global.init_async_disposable_stack_shape(shape);
    global.init_async_disposable_stack_constructor(constructor);
}

pub(super) fn initialize_dispose(cx: &Cx) {
    let name = names::dispose.to_identifier().into_string();
    let function = JsFunction::native(cx, 0, &name, dispose);
    (cx.global()).init_disposable_stack_prototype_dispose(function);
}

pub(super) fn initialize_dispose_async(cx: &Cx) {
    let function = js::function(cx, js::async_disposable_stack_prototype::disposeAsync);
    (cx.global()).init_async_disposable_stack_prototype_dispose(function);
}

#[native_abi]
fn call<K: Kind>(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    let class = K::CLASS.name();
    Err(cx.throw_type_error(format_args!(
        "calling {class} constructor without new is invalid"
    )))
}

#[native_abi]
fn construct<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let shape = cx.shape_from_new_target(frame.new_target(), &frame.callee(), K::base_shape)?;
    Ok(K::create(cx, shape, cx.new_array()))
}

fn this_stack<K: Kind>(cx: &Cx, frame: &CallFrame, method: &str) -> JsResult<Gc<Stack>> {
    let class = K::CLASS.name();
    let stack = frame.this_value().downcast::<K>().map(Gc::upcast);
    stack.ok_or_else(|| {
        cx.throw_type_error(format_args!(
            "{class}.prototype.{method} requires that |this| be a {class} object"
        ))
    })
}

fn pending_resources<K: Kind>(cx: &Cx, frame: &CallFrame, method: &str) -> JsResult<Gc<JsArray>> {
    let class = K::CLASS.name();
    let resources = this_stack::<K>(cx, frame, method)?.resources();
    resources.ok_or_else(|| {
        let message = format_args!(
            "{class}.prototype.{method} requires that |this| be a pending {class} object"
        );
        cx.throw_error(ErrorType::ReferenceError, message)
    })
}

fn require_callable<K: Kind>(cx: &Cx, callback: Value, method: &str) -> JsResult<()> {
    if callback.is_callable() {
        return Ok(());
    }
    let (class, name) = (K::CLASS.name(), K::CALLBACK);
    Err(cx.throw_type_error(format_args!(
        "{class}.prototype.{method} requires that {name} argument be a callable"
    )))
}

fn add(cx: &Cx, resources: &JsArray, value: Value, method: Value) -> JsResult<()> {
    cx.or_out_of_memory(resources.append_values(cx, &[value, method]))
}

#[native_abi]
fn adopt<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let resources = pending_resources::<K>(cx, frame, "adopt")?;
    let (value, callback) = (frame.argument(0), frame.argument(1));
    require_callable::<K>(cx, callback, "adopt")?;
    let method = JsFunctionWithFields::new(cx, 0, dispose_adopted, [callback, value]);
    add(cx, &resources, Value::UNDEFINED, method.into())?;
    Ok(value)
}

#[native_abi]
fn dispose_adopted(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let [callback, value] = JsFunctionWithFields::fields_of_callee(frame);
    cx.call(callback, Value::UNDEFINED, &[value])
}

#[native_abi]
fn defer<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let resources = pending_resources::<K>(cx, frame, "defer")?;
    require_callable::<K>(cx, frame.argument(0), "defer")?;
    add(cx, &resources, Value::UNDEFINED, frame.argument(0))?;
    Ok(Value::UNDEFINED)
}

#[native_abi]
fn r#use<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let resources = pending_resources::<K>(cx, frame, "use")?;
    let value = frame.argument(0);
    if value.is_undefined_or_null() {
        if K::IS_ASYNC {
            add(cx, &resources, Value::UNDEFINED, Value::UNDEFINED)?;
        }
    } else if value.is_object() {
        let method = get_dispose_method(cx, value, K::IS_ASYNC)?;
        add(cx, &resources, value, method)?;
    } else {
        return Err(cx.throw_type_error("Disposable value must be an object"));
    }
    Ok(value)
}

#[native_abi]
fn r#move<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let resources = pending_resources::<K>(cx, frame, "move")?;
    frame.this_value().cast::<Stack>().mark_disposed();
    Ok(K::create(cx, K::base_shape(&cx.global(), cx), resources))
}

#[native_abi]
fn disposed<K: Kind>(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let stack = this_stack::<K>(cx, frame, "disposed getter")?;
    Ok(stack.resources().is_none().into())
}

#[native_abi]
fn dispose(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let stack = this_stack::<JsDisposableStack>(cx, frame, "dispose")?;
    let Some(resources) = stack.resources() else {
        return Ok(Value::UNDEFINED);
    };
    stack.mark_disposed();
    let mut error = None;
    for index in (0..resources.length() / 2).rev() {
        let value = resources.get_index(cx, u64::from(index * 2))?;
        let method = resources.get_index(cx, u64::from(index * 2 + 1))?;
        if let Err(thrown) = cx.call(method, value, &[]) {
            let caught = cx.catch_value(thrown)?;
            error = Some(match error {
                Some(suppressed) => new_suppressed_error(cx, caught, suppressed).into(),
                None => caught,
            });
        }
    }
    match error {
        Some(error) => Err(cx.throw(error)),
        None => Ok(Value::UNDEFINED),
    }
}
