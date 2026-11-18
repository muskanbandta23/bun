//! Errors that several places throw.

use bun_engine_text::{PropertyName, StringBuilder};

use crate::{Class, Cx, ErrorInstance, ErrorType, Message, SourceAppender, Thrown, Value};

pub const TYPE_ERROR: &str = "Type error";
pub const NON_EXTENSIBLE_DEFINE: &str =
    "Attempting to define property on object that is not extensible.";
pub const READ_ONLY_PROPERTY_WRITE: &str = "Attempted to assign to readonly property.";
pub const READ_ONLY_PROPERTY_CHANGE: &str = "Attempting to change value of a readonly property.";
pub const UNABLE_TO_DELETE_PROPERTY: &str = "Unable to delete property.";
pub const UNCONFIGURABLE_CHANGE_ACCESS_MECHANISM: &str =
    "Attempting to change access mechanism for an unconfigurable property.";
pub const UNCONFIGURABLE_CHANGE_CONFIGURABILITY: &str =
    "Attempting to change configurable attribute of unconfigurable property.";
pub const UNCONFIGURABLE_CHANGE_ENUMERABILITY: &str =
    "Attempting to change enumerable attribute of unconfigurable property.";
pub const UNCONFIGURABLE_CHANGE_WRITABILITY: &str =
    "Attempting to change writable attribute of unconfigurable property.";
pub const UNCONFIGURABLE_CHANGE_SETTER: &str =
    "Attempting to change the setter of an unconfigurable property.";
pub const UNCONFIGURABLE_CHANGE_GETTER: &str =
    "Attempting to change the getter of an unconfigurable property.";
pub const PROTOTYPE_NOT_OBJECT_OR_NULL: &str = "Prototype value can only be an object or null";
pub const DEFINE_BEYOND_READ_ONLY_LENGTH: &str =
    "Attempting to define numeric property on array with non-writable length property.";
pub const INVALID_ARRAY_LENGTH: &str = "Invalid array length";
pub const INVALID_ARRAY_SIZE: &str = "Array length must be a positive integer of safe magnitude.";
pub const LENGTH_EXCEEDS_MAXIMUM: &str = "Length exceeded the maximum array length";
pub const SYMBOL_TO_STRING: &str = "Cannot convert a symbol to a string";
pub const ITERATOR_RESULT_NOT_OBJECT: &str = "Iterator result interface is not an object.";

/// Runs no script.
pub fn describe_value(cx: &Cx, value: Value, out: &mut StringBuilder) {
    if let Some(string) = value.as_string() {
        out.push_latin1(b'"');
        out.push_text(string.to_string_without_resolving().text());
        out.push_latin1(b'"');
    } else if let Some(symbol) = value.as_symbol() {
        out.push_text(symbol.descriptive_string().unwrap_or_default().text());
    } else if value.is_callable() {
        out.push_ascii("function");
    } else if let Some(object) = value.as_object() {
        // SAFETY: an inquiry runs no script, so nothing that the caller has borrowed changes.
        let cx = unsafe { Cx::from_global(cx.global()) };
        out.push_text(object.calculated_class_name(cx).text());
    } else {
        match value.primitive_to_string(cx) {
            Ok(string) => out.push_text(string.to_string_without_resolving().text()),
            Err(thrown) => cx.discard_unless_termination(thrown),
        }
    }
}

#[cold]
#[inline(never)]
fn throw_about(cx: &Cx, value: Value, message: &str, appender: SourceAppender) -> Thrown {
    let mut text = StringBuilder::new();
    describe_value(cx, value, &mut text);
    text.push_latin1(b' ');
    text.push_ascii(message);
    match text.finish() {
        Ok(text) => {
            let error = ErrorInstance::with_source(cx, ErrorType::TypeError, text, appender, value);
            cx.throw(error.into())
        }
        Err(_) => cx.throw_out_of_memory(),
    }
}

/// With the text of the expression that failed.
#[cold]
pub fn throw_type_error_with_source(cx: &Cx, message: &'static str) -> Thrown {
    let (message, appender) = (message.into_string(), SourceAppender::Default);
    let error_type = ErrorType::TypeError;
    cx.throw(ErrorInstance::with_source(cx, error_type, message, appender, Value::NULL).into())
}

pub fn throw_not_a_function(cx: &Cx, value: Value) -> Thrown {
    throw_about(cx, value, "is not a function", SourceAppender::NotAFunction)
}

pub fn throw_not_a_constructor(cx: &Cx, value: Value) -> Thrown {
    throw_about(cx, value, "is not a constructor", SourceAppender::Default)
}

pub fn throw_not_an_object(cx: &Cx, value: Value) -> Thrown {
    throw_about(cx, value, "is not an object", SourceAppender::Default)
}

pub fn throw_invalid_in_parameter(cx: &Cx, value: Value) -> Thrown {
    throw_about(cx, value, "is not an Object.", SourceAppender::In)
}

pub fn throw_instanceof_not_a_function(cx: &Cx, value: Value) -> Thrown {
    let appender = SourceAppender::InstanceofNotAFunction;
    throw_about(cx, value, " is not a function", appender)
}

pub fn throw_has_instance_not_a_function(cx: &Cx, value: Value) -> Thrown {
    let message = "[Symbol.hasInstance] is not a function, undefined, or null";
    throw_about(cx, value, message, SourceAppender::HasInstanceNotAFunction)
}

pub fn throw_invalid_prototype(cx: &Cx, value: Value) -> Thrown {
    let appender = SourceAppender::InvalidPrototype;
    throw_about(cx, value, "is not an object or null", appender)
}

/// `fallback` is for when there is no memory to put the name in.
#[cold]
pub fn throw_naming(
    cx: &Cx,
    error_type: ErrorType,
    before: &str,
    name: PropertyName<'_>,
    after: &str,
    fallback: &'static str,
) -> Thrown {
    let mut text = StringBuilder::new();
    text.push_ascii(before);
    text.push_text(name.text());
    text.push_ascii(after);
    let text = text.finish().unwrap_or_else(|_| fallback.into_string());
    cx.throw_error(error_type, text)
}

pub fn throw_undefined_variable(cx: &Cx, name: PropertyName<'_>) -> Thrown {
    let (before, after, fallback) = if name.is_private() {
        let fallback = "Can't find private variable";
        ("Can't find private variable: PrivateSymbol.", "", fallback)
    } else {
        ("", " is not defined", "Variable is not defined")
    };
    throw_naming(cx, ErrorType::ReferenceError, before, name, after, fallback)
}

pub fn throw_uninitialized_variable(cx: &Cx, name: PropertyName<'_>) -> Thrown {
    let fallback = "Cannot access uninitialized variable.";
    if name.is_symbol() || name.text().is_empty() {
        return cx.throw_reference_error(fallback);
    }
    let (before, after) = ("Cannot access '", "' before initialization.");
    throw_naming(cx, ErrorType::ReferenceError, before, name, after, fallback)
}

pub fn throw_getter_type_error(cx: &Cx, class: &Class, name: PropertyName<'_>) -> Thrown {
    let class = class.name();
    let message =
        format_args!("The {class}.{name} getter can only be used on instances of {class}");
    let error = ErrorInstance::new(cx, ErrorType::TypeError, Some(message.into_string()));
    error.set_native_getter_type_error();
    cx.throw(error.into())
}
