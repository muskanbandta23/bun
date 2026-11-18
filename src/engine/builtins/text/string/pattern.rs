//! The methods that leave it to their argument what to do, if it is an object with a method for that.

use bun_engine_text::PropertyName;
use bun_engine_vm::{CellType, ReplaceMode, errors};

use crate::prelude::*;

/// IsRegExp
pub(super) fn is_regexp(cx: &mut Cx, value: Value) -> JsResult<bool> {
    let Some(object) = value.as_object() else {
        return Ok(false);
    };
    let matcher = object.get(cx, symbols::r#match)?;
    if !matcher.is_undefined() {
        return Ok(matcher.to_boolean(cx));
    }
    Ok(object.cell_type() == CellType::RegExpObject)
}

fn require_global(cx: &mut Cx, pattern: Gc<JsObject>, message: &'static str) -> JsResult<()> {
    if is_regexp(cx, pattern.into())? {
        let flags = pattern.get(cx, names::flags)?.to_string_view(cx)?;
        if !flags.text().contains_unit(b'g'.into()) {
            return Err(cx.throw_type_error(message));
        }
    }
    Ok(())
}

fn delegate(
    cx: &mut Cx,
    pattern: Gc<JsObject>,
    name: PropertyName<'_>,
    message: &'static str,
    arguments: &[Value],
) -> JsResult<Option<Value>> {
    match pattern.as_value().get_method(cx, name, message)? {
        Some(method) => cx.call(method, pattern.into(), arguments).map(Some),
        None => Ok(None),
    }
}

fn match_with(
    cx: &mut Cx,
    frame: &CallFrame,
    name: PropertyName<'_>,
    flags: Value,
) -> JsResult<Value> {
    let (this, pattern) = (frame.this_value(), frame.argument(0));
    if let Some(object) = pattern.as_object() {
        let method = object.get(cx, name)?;
        if method.is_callable() {
            return cx.call(method, pattern, &[this]);
        }
        if !method.is_undefined_or_null() {
            return Err(errors::throw_not_a_function(cx, method));
        }
    }
    let string = this.to_string(cx)?;
    let source = if pattern.is_undefined() {
        pattern
    } else {
        pattern.to_string(cx)?.into()
    };
    let constructor = cx.regexp_constructor().into();
    let regexp = cx.construct(constructor, &[source, flags], constructor)?;
    cx.invoke(regexp.into(), name, &[string.into()])
}

#[native_abi]
pub(super) fn r#match(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.match requires that |this| not be null or undefined";
    frame.this_value().require_object_coercible(cx, message)?;
    match_with(cx, frame, symbols::r#match, Value::UNDEFINED)
}

#[native_abi]
pub(super) fn search(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.search requires that |this| not be null or undefined";
    frame.this_value().require_object_coercible(cx, message)?;
    match_with(cx, frame, symbols::search, Value::UNDEFINED)
}

#[native_abi]
pub(super) fn match_all(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.matchAll requires |this| not to be null nor undefined";
    frame.this_value().require_object_coercible(cx, message)?;
    if let Some(pattern) = frame.argument(0).as_object() {
        let message =
            "String.prototype.matchAll argument must not be a non-global regular expression";
        require_global(cx, pattern, message)?;
    }
    let flags = cx.small_strings.single_character(b'g');
    match_with(cx, frame, symbols::matchAll, flags.into())
}

#[inline(always)]
fn replace_in(cx: &mut Cx, frame: &CallFrame, mode: ReplaceMode) -> JsResult<Value> {
    let (this, search, replacement) = (frame.this_value(), frame.argument(0), frame.argument(1));
    if let Some(pattern) = search.as_object() {
        if mode == ReplaceMode::All {
            let message =
                "String.prototype.replaceAll argument must not be a non-global regular expression";
            require_global(cx, pattern, message)?;
        }
        let (message, arguments) = ("@@replace method is not callable", [this, replacement]);
        if let Some(result) = delegate(cx, pattern, symbols::replace, message, &arguments)? {
            return Ok(result);
        }
    }
    let string = this.to_string(cx)?;
    let search = search.to_string(cx)?;
    string
        .replace(cx, mode, search, replacement)
        .map(Into::into)
}

#[native_abi]
pub(super) fn replace(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.replace requires that |this| not be null or undefined";
    frame.this_value().require_object_coercible(cx, message)?;
    replace_in(cx, frame, ReplaceMode::First)
}

#[native_abi]
pub(super) fn replace_all(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.replaceAll requires |this| not to be null nor undefined";
    frame.this_value().require_object_coercible(cx, message)?;
    replace_in(cx, frame, ReplaceMode::All)
}

#[native_abi]
pub(super) fn split(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let message = "String.prototype.split requires that |this| not be null or undefined";
    let this = frame.this_value().require_object_coercible(cx, message)?;
    let (separator, limit) = (frame.argument(0), frame.argument(1));
    if let Some(pattern) = separator.as_object() {
        let message = "@@split method is not callable";
        if let Some(result) = delegate(cx, pattern, symbols::split, message, &[this, limit])? {
            return Ok(result);
        }
    }
    let string = this.to_string(cx)?;
    let limit = if limit.is_undefined() {
        u32::MAX
    } else {
        limit.to_uint32(cx)?
    };
    if separator.is_undefined() {
        let whole = [string.into()];
        return (cx.new_array_from(&whole[..limit.min(1) as usize])).map(Into::into);
    }
    let separator = separator.to_string(cx)?;
    string.split(cx, &separator, limit).map(Into::into)
}
