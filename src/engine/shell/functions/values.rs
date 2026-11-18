//! How values are represented, and values that scripts cannot make.

use bun_engine_macros::cell;
use bun_engine_vm::{
    ElementKind, GlobalObject, InternalFunction, JsArrayBuffer, PropertyDescriptor, RealmWatchpoint, Shape, ShapeSpec, TypeFlags, call,
};

use super::{return_false, return_undefined, string};
use crate::prelude::*;

pub(super) const FUNCTIONS: Functions = &[
    ("describe", describe, 1),
    ("describeArray", describe_array, 1),
    ("ensureArrayStorage", ensure_sparse_elements, 1),
    ("addressOf", address_of, 1),
    ("version", return_undefined, 1),
    ("isInt32", is_int32, 0),
    ("isPureNaN", is_pure_nan, 0),
    ("asDoubleNumber", as_double_number, 1),
    ("makeMasquerader", make_masquerader, 0),
    ("createHeapBigInt", create_heap_bigint, 1),
    ("isHeapBigInt", is_heap_bigint, 1),
    ("is8BitString", is_8_bit_string, 1),
    ("isRope", is_rope, 1),
    ("createNonRopeNonAtomString", create_plain_string, 1),
    ("is32BitPlatform", return_false, 0),
    ("maxArguments", max_arguments, 0),
    ("transferArrayBuffer", transfer_array_buffer, 1),
];

#[native_abi]
fn describe(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(&value) = frame.arguments().first() else {
        return Ok(Value::UNDEFINED);
    };
    let text = if let Some(integer) = value.as_int32() {
        format!("Int32: {integer}")
    } else if let Some(number) = value.as_double() {
        format!("Double: {}, {number:.6}", number.to_bits() as i64)
    } else if let Some(object) = value.as_object() {
        let (address, class) = (object.address(), object.class().name());
        let (shape, kind) = (object.shape().address(), object.element_kind());
        format!("Object: {address:#x} (Shape {shape:#x}:[{class}, {kind:?}])")
    } else if let Some(text) = value.as_string() {
        let kind = match (text.is_rope(), text.is_atom()) {
            (true, _) => " (rope)",
            (_, true) => " (atomic)",
            _ => "",
        };
        let width = if text.is_latin1() { "8Bit" } else { "16Bit" };
        let (length, text) = (text.len(), text.to_string_without_resolving());
        format!("String{kind},{width}:(1),length:({length}): {text}")
    } else {
        format!("{value:?}")
    };
    Ok(string(cx, &text))
}

#[native_abi]
fn describe_array(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(&value) = frame.arguments().first() else {
        return Ok(Value::UNDEFINED);
    };
    let Some(object) = value.as_object() else {
        return Ok(string(cx, "<not object>"));
    };
    let (storage, length) = (object.storage().address(), object.public_length());
    let capacity = object.vector_length();
    let text =
        format!("<Storage: {storage:#x}; public length: {length}; vector length: {capacity}>");
    Ok(string(cx, &text))
}

#[native_abi]
fn address_of(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let cell = frame.argument(0).as_cell();
    Ok(cell.map_or_default(|cell| f64::from_bits(cell.address() as u64).into()))
}

#[native_abi]
fn is_int32(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.arguments().iter().all(|it| it.is_int32()).into())
}

#[native_abi]
fn is_pure_nan(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let is_nan = |it: &Value| it.as_number().is_some_and(f64::is_nan);
    Ok(frame.arguments().iter().all(is_nan).into())
}

#[native_abi]
fn as_double_number(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    frame.argument(0).to_number(cx).map(Value::double)
}

/// An object with an `[[IsHTMLDDA]]` slot.
#[cell(name = "Function", flags = TypeFlags::MASQUERADES_AS_UNDEFINED)]
struct Masquerader {
    base: InternalFunction,
}

#[native_abi]
fn call_masquerader(_: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(Value::NULL)
}

pub(super) fn create_masquerader(cx: &Cx) -> Value {
    let (global, spec) = (cx.global(), ShapeSpec::of::<Masquerader>());
    let shape = Shape::create(cx, Some(global), cx.object_prototype().into(), spec);
    (global.watchpoints_unchecked(RealmWatchpoint::NoObjectMasqueradesAsUndefined)).invalidate(cx);
    let base = InternalFunction::init(cx, shape, call_masquerader, None);
    let function = cx.allocate::<Masquerader>(MasqueraderInit { base });
    function.define_length_and_name(cx, 0, JsString::from_literal(cx, "IsHTMLDDA"));
    function.into()
}

#[native_abi]
fn make_masquerader(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    Ok(create_masquerader(cx))
}

#[native_abi]
fn create_heap_bigint(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    frame.argument(0).to_bigint(cx).map(Into::into)
}

#[native_abi]
fn is_heap_bigint(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    Ok(frame.argument(0).is_bigint().into())
}

#[native_abi]
fn is_8_bit_string(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let string = frame.argument(0).as_string();
    Ok(string.is_some_and(|it| it.is_latin1()).into())
}

#[native_abi]
fn is_rope(_: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let string = frame.argument(0).as_string();
    Ok(string.is_some_and(|it| it.is_rope()).into())
}

#[native_abi]
fn create_plain_string(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let view = frame.argument(0).to_string_view(cx)?;
    if view.text().len() <= 1 {
        return Err(cx.throw_type_error("empty / one-length string can be always atom string"));
    }
    cx.new_string(view.text()).map(Into::into)
}

#[native_abi]
fn max_arguments(cx: &mut Cx, _: &CallFrame) -> JsResult<Value> {
    let words = (cx.stack_bounds().origin - cx.soft_stack_limit()) / size_of::<Value>();
    Ok(words.min(call::MAX_ARGUMENTS).into())
}

#[native_abi]
pub(super) fn transfer_array_buffer(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    if frame.argument_count() == 0 {
        return Err(cx.throw_error(ErrorType::Error, "Not enough arguments"));
    }
    let Some(buffer) = frame.argument(0).downcast::<JsArrayBuffer>() else {
        return Err(cx.throw_error(ErrorType::Error, "Expected an array buffer"));
    };
    if !buffer.is_shared() && buffer.buffer().is_detachable() {
        buffer.buffer().detach(cx);
    }
    Ok(Value::UNDEFINED)
}

#[native_abi]
pub(super) fn ensure_sparse_elements(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    for object in frame.arguments().iter().filter_map(|it| it.as_object()) {
        if object.class().has_ordinary_define_own_property() || object.indexing_mode().is_array() {
            cx.or_out_of_memory(object.convert_to_sparse(cx))?;
        }
    }
    Ok(Value::UNDEFINED)
}

#[native_abi]
pub(super) fn indexing_mode(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let Some(mode) = frame.argument(0).as_object().map(|it| it.indexing_mode()) else {
        return Ok(Value::UNDEFINED);
    };
    let shared = if mode.is_copy_on_write() { "CopyOnWrite" } else { "" };
    let class = if mode.is_array() { "Array" } else { "NonArray" };
    let kind = match mode.kind() {
        ElementKind::None if mode.is_array() => "Class",
        ElementKind::None => "",
        _ if mode.is_slow_put() => "WithSlowPutArrayStorage",
        ElementKind::Undecided => "WithUndecided",
        ElementKind::Int32 => "WithInt32",
        ElementKind::Double => "WithDouble",
        ElementKind::Contiguous => "WithContiguous",
        ElementKind::Sparse => "WithArrayStorage",
    };
    Ok(string(cx, &format!("{shared}{class}{kind}")))
}

fn realm_of_argument(cx: &Cx, frame: &CallFrame, function: &str) -> JsResult<Gc<GlobalObject>> {
    match frame.argument(0) {
        Value::UNDEFINED => Ok(cx.global()),
        value => (value.as_object().and_then(|it| it.realm())).ok_or_else(|| {
            cx.throw_type_error(format_args!(
                "{function} expects first argument to be an object if provided"
            ))
        }),
    }
}

#[native_abi]
pub(super) fn have_a_bad_time(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let prototype = realm_of_argument(cx, frame, "haveABadTime")?.object_prototype();
    let name = Identifier::from_index(u32::MAX - 1);
    let accessor = PropertyDescriptor::accessor(None, None, Attributes::DONT_ENUM);
    prototype.define_property_or_throw(cx, name.name(), &accessor)?;
    prototype.delete(cx, name.name()).map(Into::into)
}

#[native_abi]
pub(super) fn is_having_a_bad_time(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let realm = realm_of_argument(cx, frame, "isHavingABadTime")?;
    Ok(realm.is_having_a_bad_time().into())
}
