use bun_engine_text::AllocError;
use bun_engine_vm::{
    Bits, BufferSlice, Element, JsTypedArray, NumberElement, RepeatedCall, TypedArray,
    with_element_type,
};

use super::copy::default_shape;
use super::prototype::this_array;
use crate::arrays::merge_sort;
use crate::arrays::sort::{MIN_LENGTH_TO_AVOID_CALLS, means_less, orders_numbers_descending};
use crate::prelude::*;

/// An unsigned integer whose order is that of the elements. All NaNs have the same.
#[inline(always)]
fn key_of<T: Element>(element: T) -> T::Bits {
    let (bits, sign): (u64, u64) = (element.to_bits().into(), T::TYPE.sign_bit());
    Bits::truncate(if !T::TYPE.is_float() {
        if T::TYPE.is_signed() {
            bits ^ sign
        } else {
            bits
        }
    } else if bits & !sign > T::TYPE.bits_of_infinity() {
        T::TYPE.bits_of_nan() | sign
    } else if bits & sign != 0 {
        !bits
    } else {
        bits | sign
    })
}

#[inline(always)]
fn element_of<T: Element>(key: T::Bits) -> T {
    let (key, sign): (u64, u64) = (key.into(), T::TYPE.sign_bit());
    T::from_bits(Bits::truncate(if !T::TYPE.is_float() {
        if T::TYPE.is_signed() { key ^ sign } else { key }
    } else if key & sign != 0 {
        key ^ sign
    } else {
        !key
    }))
}

const BUCKETS: usize = 256;

#[inline(always)]
fn digit<K: Bits>(key: K, position: usize) -> usize {
    usize::from((key.into() >> (position * 8)) as u8)
}

/// There are fewer than 2^32.
fn counting_sort<T: Element>(elements: BufferSlice<'_, T>) {
    /// An increment would have to wait for the one before it if it went to the same counter.
    const WAYS: usize = 4;
    let mut counts = [[0u32; BUCKETS]; WAYS];
    let words = elements.cast::<u32>();
    let flip = u32::from_ne_bytes([digit(key_of(T::default()), 0) as u8; WAYS]);
    for word in words.iter() {
        for (counts, key) in counts.iter_mut().zip((word ^ flip).to_ne_bytes()) {
            counts[usize::from(key)] += 1;
        }
    }
    for element in elements.slice(words.len() * WAYS..elements.len()).iter() {
        counts[0][digit(key_of(element), 0)] += 1;
    }
    let mut start = 0;
    for key in 0..BUCKETS {
        let count = counts.iter().map(|it| it[key] as usize).sum::<usize>();
        if count != 0 {
            let element = element_of(Bits::truncate(key as u64));
            elements.slice(start..start + count).fill(element);
            start += count;
        }
    }
}

/// Stable. `counts` has the number of keys for each value of the digit.
#[inline(always)]
fn distribute<K: Bits>(
    keys: &[K],
    position: usize,
    counts: &[u32; BUCKETS],
    mut store: impl FnMut(usize, K),
) {
    let (mut next, mut total) = ([0; BUCKETS], 0);
    for (next, &count) in next.iter_mut().zip(counts) {
        *next = total;
        total += count;
    }
    for &key in keys {
        let next = &mut next[digit(key, position)];
        store(*next as usize, key);
        *next += 1;
    }
}

/// From the least significant byte up. There are fewer than 2^32 keys.
fn radix_sort<T: Element>(
    keys: &mut [T::Bits],
    target: BufferSlice<'_, T>,
) -> Result<(), AllocError> {
    let mut counts = [[0u32; BUCKETS]; size_of::<u64>()];
    let counts = &mut counts[..size_of::<T>()];
    for &key in &*keys {
        for (position, counts) in counts.iter_mut().enumerate() {
            counts[digit(key, position)] += 1;
        }
    }
    let (first, length) = (keys[0], keys.len());
    let varies = |&position: &usize| counts[position][digit(first, position)] as usize != length;
    let mut positions = (0..size_of::<T>()).filter(varies);
    let Some(last) = positions.next_back() else {
        target.fill(element_of(first));
        return Ok(());
    };
    let mut scratch = Vec::new();
    let mut positions = positions.peekable();
    if positions.peek().is_some() {
        scratch = T::Bits::try_zeroed_vec(length)?;
    }
    let (mut from, mut to) = (keys, &mut *scratch);
    for position in positions {
        distribute(from, position, &counts[position], |index, key| {
            to[index] = key;
        });
        core::mem::swap(&mut from, &mut to);
    }
    distribute(from, last, &counts[last], |index, key| {
        target.set(index, element_of(key));
    });
    Ok(())
}

/// Below this many elements, comparing them takes less time than counting.
const fn min_length_for_radix_sort(element_size: usize) -> usize {
    match element_size {
        1 => 128,
        2 => 256,
        4 => 1024,
        _ => usize::MAX,
    }
}

/// Reads each element once and writes it once, whatever another thread does meanwhile.
fn sort_numerically<T: Element>(elements: BufferSlice<'_, T>) -> Result<(), AllocError> {
    const ON_STACK: usize = 64;
    let length = elements.len();
    let counts = (min_length_for_radix_sort(size_of::<T>())..=u32::MAX as usize).contains(&length);
    if counts && size_of::<T>() == 1 {
        counting_sort(elements);
        return Ok(());
    }
    let (mut short, mut long) = ([T::Bits::default(); ON_STACK], None);
    let keys = if length <= ON_STACK {
        elements.bits().copy_to_slice(&mut short[..length]);
        &mut short[..length]
    } else {
        &mut **long.insert(elements.bits().try_to_vec()?)
    };
    for key in &mut *keys {
        *key = key_of(T::from_bits(*key));
    }
    if !counts || keys.is_sorted() || radix_sort(keys, elements).is_err() {
        keys.sort_unstable();
        elements.fill_from(keys.iter().map(|&key| element_of(key)));
    }
    Ok(())
}

fn sort_by<T: Element>(cx: &mut Cx, array: &TypedArray<T>, comparator: Value) -> JsResult<()> {
    let items = array.elements(&cx.no_gc()).bits().try_to_vec();
    let mut items = cx.or_out_of_memory(items)?;
    let mut scratch = cx.or_out_of_memory(T::Bits::try_zeroed_vec(items.len()))?;
    let comparator = RepeatedCall::new(cx, comparator)?;
    merge_sort::sort(&mut items, &mut scratch, |a, b| {
        let arguments = [T::from_bits(a).to_value(cx), T::from_bits(b).to_value(cx)];
        let result = comparator.call(cx, Value::UNDEFINED, arguments)?;
        means_less(cx, result)
    })?;
    let no_gc = cx.no_gc();
    let target = array.elements(&no_gc).bits().clamped(0..items.len());
    target.copy_from_slice(&items[..target.len()]);
    Ok(())
}

/// Whether arithmetic puts the elements in the order of `sort_numerically`, and tells apart those that differ.
fn has_only_ordinary_numbers(cx: &mut Cx, array: &JsTypedArray) -> bool {
    let is_ordinary = |number: f64| !number.is_nan() && number.to_bits() != (-0.0f64).to_bits();
    with_element_type!(array.element_type(),
        T => array.typed::<T>().elements(&cx.no_gc()).iter().all(|it| is_ordinary(it.convert())),
        bigint => false
    )
}

fn sort_in_place(cx: &mut Cx, array: &JsTypedArray, comparator: Value) -> JsResult<()> {
    let is_descending = if comparator.is_undefined() {
        Some(false)
    } else if array.length() >= MIN_LENGTH_TO_AVOID_CALLS {
        orders_numbers_descending(cx, comparator)?.filter(|_| has_only_ordinary_numbers(cx, array))
    } else {
        None
    };
    with_element_type!(array.element_type(), T => {
        let Some(is_descending) = is_descending else {
            return sort_by(cx, array.typed::<T>(), comparator);
        };
        let no_gc = cx.no_gc();
        let elements = array.typed::<T>().elements(&no_gc);
        let result = sort_numerically(elements);
        if is_descending {
            elements.reverse();
        }
        cx.or_out_of_memory(result)
    })
}

fn check_comparator(cx: &Cx, comparator: Value, message: &'static str) -> JsResult<()> {
    if !comparator.is_undefined() && !comparator.is_callable() {
        return Err(cx.throw_type_error(message));
    }
    Ok(())
}

#[native_abi]
pub(super) fn sort(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (this, comparator) = (this_array(cx, frame)?, frame.argument(0));
    let message =
        "TypedArray.prototype.sort requires the comparator argument to be a function or undefined";
    check_comparator(cx, comparator, message)?;
    this.validate(cx)?;
    sort_in_place(cx, &this, comparator)?;
    Ok(this.into())
}

#[native_abi]
pub(super) fn to_sorted(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (this, comparator) = (this_array(cx, frame)?, frame.argument(0));
    let message = "TypedArray.prototype.toSorted requires the comparator argument to be a function or undefined";
    check_comparator(cx, comparator, message)?;
    let length = this.validate(cx)?;
    let result = with_element_type!(this.element_type(), T => {
        TypedArray::<T>::create_filled(cx, default_shape::<T>(cx), length, |no_gc, filler| {
            filler.copy(this.typed().elements(no_gc).slice(0..length));
        })?
        .upcast::<JsTypedArray>()
    });
    sort_in_place(cx, &result, comparator)?;
    Ok(result.into())
}
