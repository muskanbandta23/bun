use core::ops::Range;

use bun_engine_vm::{Direction, Elements, SearchResult};

use super::this_object;
use crate::prelude::*;

/// `None` if it takes more than a look at the elements.
#[inline(always)]
fn search_elements(
    cx: &mut Cx,
    object: &JsObject,
    mut range: Range<u64>,
    needle: Value,
    direction: Direction,
    find: impl Fn(Elements<'_>) -> SearchResult,
) -> JsResult<Option<Option<u64>>> {
    if let Some(string) = needle.as_string() {
        string.view(cx)?;
    }
    loop {
        let no_gc = cx.no_gc();
        let Some(elements) = object.plain_elements(&no_gc, range.clone()) else {
            return Ok(None);
        };
        let offset = match find(elements) {
            SearchResult::Found(offset) => return Ok(Some(Some(range.start + offset as u64))),
            SearchResult::NotFound => return Ok(Some(None)),
            SearchResult::Unresolved(offset) => offset,
        };
        if let Some(rope) = elements.at(offset).and_then(Value::as_string) {
            rope.resolve(cx)?;
        }
        match direction {
            Direction::Forward => range.start += offset as u64,
            Direction::Backward => range.end = range.start + offset as u64 + 1,
        }
    }
}

#[inline(always)]
fn find_index<const DIRECTION: Direction>(
    cx: &mut Cx,
    object: &JsObject,
    mut range: Range<u64>,
    needle: Value,
) -> JsResult<Value> {
    let find = |elements: Elements<'_>| elements.find_strictly_equal::<DIRECTION>(needle);
    if let Some(index) = search_elements(cx, object, range.clone(), needle, DIRECTION, find)? {
        return Ok(index.map_or(Value::int32(-1), Value::from));
    }
    while let Some(index) = match DIRECTION {
        Direction::Forward => range.next(),
        Direction::Backward => range.next_back(),
    } {
        if let Some(element) = object.get_index_if_has_property(cx, index)?
            && needle.strict_equals(cx, element)?
        {
            return Ok(index.into());
        }
        cx.check_traps()?;
    }
    Ok(Value::int32(-1))
}

#[native_abi]
pub(super) fn index_of(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    let length = object.length_of_array_like(cx)?;
    if length == 0 {
        return Ok(Value::int32(-1));
    }
    let start = frame.argument(1).to_relative_index(cx, length, 0)?;
    find_index::<{ Direction::Forward }>(cx, &object, start..length, frame.argument(0))
}

#[native_abi]
pub(super) fn last_index_of(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = frame.this_value().to_object(cx)?;
    let length = object.length_of_array_like(cx)?;
    if length == 0 {
        return Ok(Value::int32(-1));
    }
    let mut end = length;
    if let Some(from) = frame.arguments().get(1) {
        let from = from.to_integer_or_infinity(cx)?;
        let last = if from < 0.0 { from + length as f64 } else { from };
        end = ((last + 1.0) as u64).min(length);
    }
    find_index::<{ Direction::Backward }>(cx, &object, 0..end, frame.argument(0))
}

#[native_abi]
pub(super) fn includes(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let object = this_object!(cx, frame, "includes")?;
    let length = object.length_of_array_like(cx)?;
    if length == 0 {
        return Ok(Value::FALSE);
    }
    let range = frame.argument(1).to_relative_index(cx, length, 0)?..length;
    let needle = frame.argument(0);
    let find = |elements: Elements<'_>| elements.find_same_value_zero(needle);
    let direction = Direction::Forward;
    if let Some(index) = search_elements(cx, &object, range.clone(), needle, direction, find)? {
        return Ok(index.is_some().into());
    }
    for index in range {
        let element = object.get_index(cx, index)?;
        if needle.same_value_zero(cx, element)? {
            return Ok(Value::TRUE);
        }
        cx.check_traps()?;
    }
    Ok(Value::FALSE)
}
