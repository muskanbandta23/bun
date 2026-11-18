use bun_engine_number::NumberText;
use bun_engine_text::AllocError;
use bun_engine_vm::{ElementKind, HoleMode, JsArray, RootVec, root_vec};

use super::copy::{array_create, check_array_size};
use super::string_sort::{int32_of_key, key_of_int32, sorted_indices};
use super::{merge_sort, this_object};
use crate::prelude::*;

/// Whether what a comparator has returned puts its first argument before the second.
#[inline(always)]
pub(crate) fn means_less(cx: &mut Cx, result: Value) -> JsResult<bool> {
    if let Some(number) = result.as_int32() {
        return Ok(number < 0);
    }
    // For comparators such as `(a, b) => a > b`.
    if result.is_boolean() {
        return Ok(result.is_false());
    }
    Ok(result.to_number(cx)? < 0.0)
}

const INLINE_CAPACITY: usize = 64;

/// A list with a limit that is known when it is made. A short one allocates nothing.
struct Buffer {
    inline: [Value; INLINE_CAPACITY],
    spilled: RootVec<Value>,
}

impl Buffer {
    #[inline(always)]
    fn new() -> Buffer {
        Buffer {
            inline: [Value::UNDEFINED; INLINE_CAPACITY],
            spilled: root_vec(),
        }
    }

    #[inline(always)]
    fn room(&mut self, length: usize) -> Result<&mut [Value], AllocError> {
        if length <= INLINE_CAPACITY {
            return Ok(&mut self.inline[..length]);
        }
        (self.spilled.try_reserve_exact(length)).map_err(|_| AllocError)?;
        self.spilled.resize(length, Value::UNDEFINED);
        Ok(&mut self.spilled)
    }
}

/// What there is to sort.
struct Collected<'a> {
    values: &'a mut [Value],
    kind: ElementKind,
    undefined_count: u64,
}

fn collect<'a>(
    cx: &mut Cx,
    object: &JsObject,
    length: u64,
    buffer: &'a mut Buffer,
) -> JsResult<Collected<'a>> {
    let mut undefined_count = 0;
    let no_gc = cx.no_gc();
    if let Some(elements) = object.plain_elements(&no_gc, 0..length) {
        let (kind, mut count) = (elements.kind(), 0);
        let Ok(room) = buffer.room(elements.len()) else {
            return Err(cx.throw_out_of_memory());
        };
        for value in elements.iter().flatten() {
            room[count] = value;
            count += usize::from(!value.is_undefined());
            undefined_count += u64::from(value.is_undefined());
        }
        return Ok(Collected {
            values: &mut room[..count],
            kind,
            undefined_count,
        });
    }
    let values = &mut buffer.spilled;
    for index in 0..length {
        match object.get_index_if_has_property(cx, index)? {
            Some(Value::UNDEFINED) => undefined_count += 1,
            Some(value) => {
                if values.len() == u32::MAX as usize || values.try_reserve(1).is_err() {
                    return Err(cx.throw_out_of_memory());
                }
                values.push(value);
            }
            None => cx.check_traps()?,
        }
    }
    Ok(Collected {
        values,
        kind: ElementKind::Contiguous,
        undefined_count,
    })
}

fn try_vec<T>(items: impl ExactSizeIterator<Item = T>) -> Result<Vec<T>, AllocError> {
    let mut list = Vec::new();
    list.try_reserve_exact(items.len()).map_err(|_| AllocError)?;
    list.extend(items);
    Ok(list)
}

fn sort_int32s_as_strings(values: &mut [Value]) -> Result<(), AllocError> {
    let mut keys = try_vec(values.iter().map(|it| key_of_int32(it.to_bits() as i32)))?;
    keys.sort_unstable();
    for (value, key) in values.iter_mut().zip(keys) {
        *value = Value::int32(int32_of_key(key));
    }
    Ok(())
}

fn permute(values: &mut [Value], scratch: &mut [Value], order: impl Iterator<Item = u32>) {
    scratch.copy_from_slice(values);
    for (value, index) in values.iter_mut().zip(order) {
        *value = scratch[index as usize];
    }
}

fn sort_doubles_as_strings(values: &mut [Value], scratch: &mut [Value]) -> Result<(), AllocError> {
    let texts = try_vec((values.iter()).map(|it| NumberText::from(it.double_unchecked())))?;
    let order = sorted_indices(values.len() as u32, |index| texts[index as usize].text())?;
    permute(values, scratch, order);
    Ok(())
}

fn sort_as_strings(cx: &mut Cx, values: &mut [Value], scratch: &mut [Value]) -> JsResult<()> {
    let mut views = root_vec();
    if views.try_reserve_exact(values.len()).is_err() {
        return Err(cx.throw_out_of_memory());
    }
    for value in &*values {
        views.push(value.to_string_view(cx)?);
    }
    let order = sorted_indices(values.len() as u32, |index| views[index as usize].text());
    permute(values, scratch, cx.or_out_of_memory(order)?);
    Ok(())
}

fn sort_values(cx: &mut Cx, values: &mut [Value], kind: ElementKind, order: Value) -> JsResult<()> {
    if order.is_undefined() && kind == ElementKind::Int32 {
        return cx.or_out_of_memory(sort_int32s_as_strings(values));
    }
    let mut scratch = Buffer::new();
    let scratch = cx.or_out_of_memory(scratch.room(values.len()))?;
    if !order.is_undefined() {
        return merge_sort::sort(values, scratch, |a, b| {
            let result = cx.call(order, Value::UNDEFINED, &[a, b])?;
            means_less(cx, result)
        });
    }
    if kind == ElementKind::Double {
        return cx.or_out_of_memory(sort_doubles_as_strings(values, scratch));
    }
    sort_as_strings(cx, values, scratch)
}

fn sort_object(cx: &mut Cx, object: &JsObject, length: u64, comparator: Value) -> JsResult<()> {
    if length < 2 {
        return Ok(());
    }
    let mut buffer = Buffer::new();
    let Collected {
        values,
        kind,
        undefined_count,
    } = collect(cx, object, length, &mut buffer)?;
    sort_values(cx, values, kind, comparator)?;

    if !cx.or_out_of_memory(object.try_set_elements(cx, 0, values))? {
        for (index, &value) in (0..).zip(&*values) {
            object.set_index(cx, index, value, true)?;
        }
    }
    let first_undefined = values.len() as u64;
    let first_hole = first_undefined + undefined_count;
    for index in first_undefined..first_hole {
        object.set_index(cx, index, Value::UNDEFINED, true)?;
    }
    for index in first_hole..length {
        object.delete_index_or_throw(cx, index)?;
        cx.check_traps()?;
    }
    Ok(())
}

#[native_abi]
pub(super) fn sort(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let comparator = frame.argument(0);
    if !comparator.is_undefined() && !comparator.is_callable() {
        return Err(cx.throw_type_error(
            "Array.prototype.sort requires the comparator argument to be a function or undefined",
        ));
    }
    let object = frame.this_value().to_object(cx)?;
    let length = object.length_of_array_like(cx)?;
    sort_object(cx, &object, length, comparator)?;
    Ok(object.into())
}

#[native_abi]
pub(super) fn to_sorted(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let comparator = frame.argument(0);
    if !comparator.is_undefined() && !comparator.is_callable() {
        return Err(cx.throw_type_error(
            "Array.prototype.toSorted requires the comparator argument to be a function or undefined",
        ));
    }
    let object = this_object!(cx, frame, "toSorted")?;
    let length = object.length_of_array_like(cx)?;
    let size = check_array_size(cx, length)?;
    let holes = HoleMode::FillWithUndefined;
    let result = match JsArray::try_copy(cx, &object, 0..length, holes)? {
        Some(copy) => copy,
        None => array_create(cx, size, |cx, index| object.get_index(cx, index))?,
    };
    sort_object(cx, &result, length, comparator)?;
    Ok(result.into())
}
