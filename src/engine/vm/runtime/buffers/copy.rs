//! Storing many elements at once.

use core::ops::Range;

use super::create::Filler;
use super::element::{Element, ElementType, NumberElement};
use super::slice::BufferSlice;
use crate::{
    Cx, Elements, Gc, JsArrayBufferView, JsObject, JsResult, JsTypedArray, NoGc, Shape, TypeFlags,
    TypedArray, Value, errors, with_element_type,
};

/// What counts if the source and the target overlap.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CopyOrder {
    /// An element is read after those before it have been written.
    Ascending,
    /// All are read before any is written.
    AtOnce,
}

/// A type that a value is converted to in the same way, so that the two share their code.
#[inline(always)]
fn unsigned(to: ElementType) -> ElementType {
    match to {
        ElementType::Int8 => ElementType::Uint8,
        ElementType::Int16 => ElementType::Uint16,
        ElementType::Int32 => ElementType::Uint32,
        ElementType::BigInt64 => ElementType::BigUint64,
        other => other,
    }
}

/// From numbers to numbers.
fn convert(
    target: BufferSlice<'_, u8>,
    to: ElementType,
    source: BufferSlice<'_, u8>,
    from: ElementType,
) {
    #[inline(always)]
    fn convert_to<T: NumberElement>(
        target: BufferSlice<'_, T>,
        source: BufferSlice<'_, u8>,
        from: ElementType,
    ) {
        let from = match from {
            ElementType::Uint8Clamped => ElementType::Uint8,
            other => other,
        };
        with_element_type!(from,
            S => target.convert_ascending_from(source.cast::<S>()),
            bigint => unreachable!()
        );
    }
    with_element_type!(unsigned(to),
        T => convert_to(target.cast::<T>(), source, from),
        bigint => unreachable!()
    );
}

/// These elements, if getting them comes down to reading the storage.
#[inline]
fn plain_elements<'a>(
    source: &'a JsObject,
    range: &Range<usize>,
    no_gc: &'a NoGc<'_>,
) -> Option<Elements<'a>> {
    let flags = source.shape().type_flags();
    if flags.contains(TypeFlags::INTERCEPTS_INDEXED_ACCESS) {
        return None;
    }
    source.plain_elements(no_gc, range.start as u64..range.end as u64)
}

/// Up to the first whose conversion takes more than computing. Returns how many it has stored.
#[inline]
fn store_numerics<T: Element>(target: BufferSlice<'_, T>, values: &[Option<Value>]) -> usize {
    target.fill_from(values.iter().map_while(|&value| T::from_numeric(value?)))
}

/// The same.
fn store_plain_elements(
    target: BufferSlice<'_, u8>,
    to: ElementType,
    elements: Elements<'_>,
) -> usize {
    #[inline(always)]
    fn store<T: NumberElement>(target: BufferSlice<'_, T>, elements: Elements<'_>) -> usize {
        match elements {
            Elements::Int32(values) => {
                let hole = T::from_double(f64::NAN);
                target.fill_from(values.iter().map(|&value| {
                    let bits = Value::bits_of(value);
                    // The low half of nothing is zero, which is what an integer makes of undefined.
                    if T::TYPE.is_float() && bits == 0 {
                        hole
                    } else {
                        T::from_int32(bits as i32)
                    }
                }))
            }
            Elements::Double(numbers) => {
                target.fill_from(numbers.iter().map(|&number| T::from_double(number)))
            }
            Elements::Contiguous(values) => store_numerics(target, values),
        }
    }
    with_element_type!(unsigned(to),
        T => store(target.cast::<T>(), elements),
        bigint T => match elements {
            Elements::Contiguous(values) => store_numerics(target.cast::<T>(), values),
            _ => 0,
        }
    )
}

impl<T: Element> Filler<'_, T> {
    /// These elements of `source`, which has the same content type. Panics if it does not have them or they do not fit.
    pub fn convert(&mut self, source: &JsTypedArray, range: Range<usize>, no_gc: &NoGc<'_>) {
        let (from, shift) = (source.element_type(), source.size_shift());
        assert!(from.is_bigint() == T::TYPE.is_bigint());
        let target = self.reserve(range.len()).bytes();
        let source = source
            .bytes(no_gc)
            .slice(range.start << shift..range.end << shift);
        if T::TYPE.has_bits_of(from) {
            target.copy_from(source);
        } else {
            convert(target, T::TYPE, source, from);
        }
    }
}

impl<T: Element> TypedArray<T> {
    /// An array of that length that has as many of the first elements of `source` as it takes no code to get and
    /// convert, and their number. The others are zero.
    #[inline]
    pub fn from_plain_elements(
        cx: &mut Cx,
        shape: Gc<Shape>,
        source: &JsObject,
        length: usize,
    ) -> JsResult<(Gc<TypedArray<T>>, usize)> {
        let mut count = 0;
        let array = Self::create_filled(cx, shape, length, |no_gc, filler| {
            if let Some(elements) = plain_elements(source, &(0..length), no_gc) {
                count =
                    filler.write_some(|rest| store_plain_elements(rest.bytes(), T::TYPE, elements));
            }
        })?;
        Ok((array, count))
    }

    /// From `offset` on, as far as the array goes at the time of each.
    pub fn set_values(&self, cx: &mut Cx, offset: usize, values: &[Value]) -> JsResult<()> {
        for (index, &value) in (offset..).zip(values) {
            self.set_value(cx, index, value)?;
        }
        Ok(())
    }

    /// The same for what it gets from `source`.
    pub fn set_one_by_one(
        &self,
        cx: &mut Cx,
        offset: usize,
        source: &JsObject,
        range: Range<usize>,
    ) -> JsResult<()> {
        for (to, from) in (offset..).zip(range) {
            let value = source.get_index(cx, from as u64)?;
            self.set_value(cx, to, value)?;
        }
        Ok(())
    }
}

impl JsTypedArray {
    #[inline(always)]
    fn check_range(cx: &Cx, length: usize, offset: usize, count: usize) -> JsResult<()> {
        if offset > length || count > length - offset {
            return Err(cx.throw_range_error(errors::RANGE_OUT_OF_BOUNDS));
        }
        Ok(())
    }

    /// Stores as many of these elements of `source` as it has, from `offset` on.
    #[inline]
    pub fn set_from_typed_array(
        &self,
        cx: &mut Cx,
        offset: usize,
        source: &JsTypedArray,
        range: Range<usize>,
        order: CopyOrder,
    ) -> JsResult<()> {
        let end = range.end.min(source.length());
        let (start, count) = (range.start.min(end), end.saturating_sub(range.start));
        Self::check_range(cx, self.length(), offset, count)?;
        let (to, from) = (self.element_type(), source.element_type());
        if to.is_bigint() != from.is_bigint() {
            return Err(cx.throw_type_error(errors::CONTENT_TYPES_DIFFER));
        }
        let no_gc = cx.no_gc();
        let (to_shift, from_shift) = (self.size_shift(), source.size_shift());
        let target = self
            .bytes(&no_gc)
            .slice(offset << to_shift..(offset + count) << to_shift);
        let source = source
            .bytes(&no_gc)
            .slice(start << from_shift..(start + count) << from_shift);
        if to.has_bits_of(from) {
            match order {
                CopyOrder::Ascending => target.copy_ascending_from(source),
                CopyOrder::AtOnce => target.copy_from(source),
            }
        } else if order == CopyOrder::Ascending
            || !target.overlaps(source)
            || (to_shift <= from_shift && target.as_ptr() <= source.as_ptr())
        {
            convert(target, to, source, from);
        } else {
            let Ok(mut copy) = source.try_to_vec() else {
                return Err(cx.throw_out_of_memory());
            };
            convert(target, to, BufferSlice::from(&mut *copy), from);
        }
        Ok(())
    }

    /// Gets these elements of `source` and sets them from `offset` on.
    pub fn set_from_object(
        &self,
        cx: &mut Cx,
        offset: usize,
        source: &JsObject,
        range: Range<usize>,
    ) -> JsResult<()> {
        Self::check_range(cx, self.length(), offset, range.len())?;
        self.set_from_object_in_range(cx, offset, source, range)
    }

    fn set_from_object_in_range(
        &self,
        cx: &mut Cx,
        offset: usize,
        source: &JsObject,
        range: Range<usize>,
    ) -> JsResult<()> {
        let (no_gc, shift) = (cx.no_gc(), self.size_shift());
        let mut count = 0;
        if let Some(elements) = plain_elements(source, &range, &no_gc)
            && let Some(target) =
                (self.bytes(&no_gc)).try_slice(offset << shift, range.len() << shift)
        {
            count = store_plain_elements(target, self.element_type(), elements);
        }
        let (offset, range) = (offset + count, range.start + count..range.end);
        if range.is_empty() {
            return Ok(());
        }
        with_element_type!(self.element_type(), T => {
            self.typed::<T>().set_one_by_one(cx, offset, source, range)
        })
    }

    /// SetTypedArrayFromArrayLike. An offset that is too large for the type is its maximum.
    pub fn set_from_array_like(&self, cx: &mut Cx, offset: usize, source: Value) -> JsResult<()> {
        let length = self.validate(cx)?;
        let source = source.to_object(cx)?;
        let count = source.length_of_array_like(cx)? as usize;
        Self::check_range(cx, length, offset, count)?;
        self.set_from_object_in_range(cx, offset, &source, 0..count)
    }

    /// SetTypedArrayFromTypedArray. The same holds for the offset.
    #[inline]
    pub fn set_from_whole_typed_array(
        &self,
        cx: &mut Cx,
        offset: usize,
        source: &JsTypedArray,
    ) -> JsResult<()> {
        if self.is_out_of_bounds() {
            return Err(JsArrayBufferView::throw_out_of_bounds(cx));
        }
        let count = source.validate(cx)?;
        self.set_from_typed_array(cx, offset, source, 0..count, CopyOrder::AtOnce)
    }
}
