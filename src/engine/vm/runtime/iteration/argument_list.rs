//! The arguments of a call whose number is known only when it is made.

use core::mem::MaybeUninit;

use super::new_array::as_spread;
use crate::call::MAX_ARGUMENTS;
use crate::{
    CellType, Cx, Elements, Gc, ImmutableArray, JsArrayBufferView, JsObject, JsResult, NoGc, Value,
    ValueVec, errors,
};

fn number_view(object: &JsObject) -> Option<Gc<JsArrayBufferView>> {
    let cell_type = object.cell_type();
    (CellType::Int8Array..=CellType::Float64Array)
        .contains(&cell_type)
        .then(|| object.as_object().cast())
}

/// `to_argument_list`, of a value that is not empty.
#[inline]
pub fn to_argument_list(cx: &mut Cx, value: Value) -> JsResult<Value> {
    let Some(object) = value.as_object() else {
        if value.is_undefined_or_null() {
            return Ok(value);
        }
        let message = "second argument to Function.prototype.apply must be an Array-like object";
        return Err(errors::throw_type_error_with_source(cx, message));
    };
    let length = object.length_of_array_like(cx)?;
    if length > MAX_ARGUMENTS as u64 {
        return Err(cx.throw_stack_overflow());
    }
    let is_readable = match number_view(&object) {
        Some(view) => view.length() as u64 == length,
        None => {
            u64::from(object.public_length()) == length
                && object.plain_elements(&cx.no_gc(), 0..length).is_some()
        }
    };
    if is_readable {
        return Ok(value);
    }
    let values = get_elements(cx, &object, length)?;
    cx.or_out_of_memory(ImmutableArray::try_from_values(cx, &values))
        .map(Into::into)
}

fn get_elements(cx: &mut Cx, object: &JsObject, length: u64) -> JsResult<ValueVec> {
    let mut values = ValueVec::new();
    cx.or_out_of_memory(values.try_reserve(length as usize))?;
    for index in 0..length {
        values.push(object.get_index(cx, index)?);
    }
    Ok(values)
}

/// CreateListFromArrayLike, for a call.
pub fn arguments_from_array_like(cx: &mut Cx, object: &JsObject) -> JsResult<ValueVec> {
    let length = object.length_of_array_like(cx)?;
    if length > MAX_ARGUMENTS as u64 {
        return Err(cx.throw_stack_overflow());
    }
    get_elements(cx, object, length)
}

/// Of what `call_varargs` and its kin get, unless that is empty. The first `skip` do not count.
#[inline]
pub fn argument_list_length(list: Value, skip: u32) -> usize {
    let length = if let Some(list) = list.downcast::<ImmutableArray>() {
        list.len() as usize
    } else if let Some(object) = list.as_object() {
        number_view(&object).map_or(object.public_length() as usize, |view| view.length())
    } else {
        0
    };
    length.saturating_sub(skip as usize)
}

#[inline(always)]
fn load_elements(elements: Elements<'_>, target: &mut [MaybeUninit<Value>]) {
    match elements {
        Elements::Int32(values) | Elements::Contiguous(values) => {
            for (target, value) in target.iter_mut().zip(values) {
                target.write(value.unwrap_or_default());
            }
        }
        Elements::Double(_) => {
            for (target, value) in target.iter_mut().zip(elements.iter()) {
                target.write(value.unwrap_or_default());
            }
        }
    }
}

/// Fills `target`, whose length is what `argument_list_length` has just said.
#[inline]
pub fn load_argument_list(
    no_gc: &NoGc<'_>,
    list: Value,
    skip: u32,
    target: &mut [MaybeUninit<Value>],
) {
    if target.is_empty() {
        return;
    }
    let range = skip as usize..skip as usize + target.len();
    if let Some(list) = list.downcast::<ImmutableArray>() {
        return load_elements(list.elements().slice(range), target);
    }
    let object = list.cast::<JsObject>();
    match number_view(&object) {
        Some(view) => {
            for (target, index) in target.iter_mut().zip(range) {
                target.write(view.try_get_number(index).unwrap_or_default());
            }
        }
        None => load_elements(object.dense_elements(no_gc).slice(range), target),
    }
}

/// For `call_with_spread`. Fills `target`. Panics unless its length is `spread_length`.
#[inline]
pub fn load_spread_arguments(registers: &[Option<Value>], target: &mut [MaybeUninit<Value>]) {
    let mut rest = target;
    for &register in registers.iter().rev() {
        let list = as_spread(register);
        let (taken, after) = rest.split_at_mut(list.map_or(1, |list| list.len() as usize));
        match list {
            Some(list) => load_elements(list.elements(), taken),
            None => {
                taken[0].write(register.unwrap_or_default());
            }
        }
        rest = after;
    }
    assert!(rest.is_empty());
}
