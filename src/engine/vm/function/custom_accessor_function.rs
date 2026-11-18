//! The functions that scripts see for the getter and the setter of a custom accessor.

use bun_engine_bytecode::Visibility;
use bun_engine_macros::{cell, native_abi};
use bun_engine_text::{Identifier, PropertyName, StringRef, names};

use crate::call::native_not_a_constructor;
use crate::object::function_name;
use crate::{
    Attributes, CallFrame, Class, CustomGetter, CustomSetter, Cx, Gc, Intrinsic, JsFunction,
    JsResult, JsString, NativeExecutable, NativeFunction, Value, errors,
};

#[cell(name = "Function")]
pub struct JsCustomGetterFunction {
    base: JsFunction,
    property: Identifier,
    getter: CustomGetter,
    required_class: Option<&'static Class>,
}

#[cell(name = "Function")]
pub struct JsCustomSetterFunction {
    base: JsFunction,
    property: Identifier,
    setter: CustomSetter,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct CustomAccessorKey {
    global: usize,
    property: Identifier,
    function: usize,
    required_class: usize,
    is_getter: bool,
}

// SAFETY: integers and an identifier.
unsafe impl crate::heap::PlainKey for CustomAccessorKey {}

fn executable(
    cx: &Cx,
    name: PropertyName<'_>,
    length: u32,
    function: NativeFunction,
) -> Gc<NativeExecutable> {
    // What `toString` returns has the syntax of NativeFunction, which a name such as `$*` does not fit in.
    let is_in_name = |unit: u16| {
        u8::try_from(unit).is_ok_and(|it| it.is_ascii_alphanumeric() || b"$_".contains(&it))
            || unit >= 0x80
    };
    let name = match name.is_symbol() || name.text().code_units().all(is_in_name) {
        true => function_name("", name),
        false => StringRef::empty(),
    };
    let constructor = native_not_a_constructor;
    let (intrinsic, visibility) = (Intrinsic::None, Visibility::Public);
    NativeExecutable::shared(
        cx,
        length,
        name,
        function,
        constructor,
        intrinsic,
        visibility,
    )
}

fn cached(
    cx: &Cx,
    key: CustomAccessorKey,
    make: impl FnOnce() -> Gc<JsFunction>,
) -> Gc<JsFunction> {
    let cache = &cx.custom_accessor_functions;
    cache.get(cx, &key).unwrap_or_else(|| {
        let function = make();
        // Only the property has the prefix, and not the source text.
        let prefix = if key.is_getter { "get " } else { "set " };
        let name = JsString::new(cx, function_name(prefix, key.property.name())).into();
        let attributes = Attributes::READ_ONLY | Attributes::DONT_ENUM;
        function.put_direct(cx, names::name, name, attributes);
        cache.insert(key, function);
        function
    })
}

impl JsFunction {
    pub fn for_custom_getter(
        cx: &Cx,
        name: PropertyName<'_>,
        getter: CustomGetter,
        required_class: Option<&'static Class>,
    ) -> Gc<JsFunction> {
        let key = CustomAccessorKey {
            global: cx.global().address(),
            property: name.to_identifier(),
            function: getter as usize,
            required_class: required_class.map_or(0, |class| core::ptr::from_ref(class).addr()),
            is_getter: true,
        };
        cached(cx, key, || {
            let executable = executable(cx, name, 0, call_getter);
            cx.allocate::<JsCustomGetterFunction>(JsCustomGetterFunctionInit {
                base: Self::native_init(cx, cx.custom_getter_function_shape(), executable),
                property: name.to_identifier(),
                getter,
                required_class,
            })
            .upcast()
        })
    }

    pub fn for_custom_setter(
        cx: &Cx,
        name: PropertyName<'_>,
        setter: CustomSetter,
    ) -> Gc<JsFunction> {
        let key = CustomAccessorKey {
            global: cx.global().address(),
            property: name.to_identifier(),
            function: setter as usize,
            required_class: 0,
            is_getter: false,
        };
        cached(cx, key, || {
            let executable = executable(cx, name, 1, call_setter);
            cx.allocate::<JsCustomSetterFunction>(JsCustomSetterFunctionInit {
                base: Self::native_init(cx, cx.custom_setter_function_shape(), executable),
                property: name.to_identifier(),
                setter,
            })
            .upcast()
        })
    }
}

#[native_abi]
fn call_getter(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let function = frame.callee().cast::<JsCustomGetterFunction>();
    let (this, name) = (frame.this_value(), function.anchor().property.name());
    if let Some(class) = function.required_class
        && !this.as_cell().is_some_and(|cell| cell.inherits(class))
    {
        return Err(errors::throw_getter_type_error(cx, class, name));
    }
    let result = (function.getter)(cx, this, name);
    cx.check_exception()?;
    Ok(result.unwrap_or_default())
}

#[native_abi]
fn call_setter(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let function = frame.callee().cast::<JsCustomSetterFunction>();
    let name = function.anchor().property.name();
    (function.setter)(cx, frame.this_value(), frame.argument(0), name);
    cx.check_exception()?;
    Ok(Value::UNDEFINED)
}
