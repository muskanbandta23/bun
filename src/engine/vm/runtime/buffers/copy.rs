//! Storing many elements at once.

use core::ops::Range;

use super::create::Filler;
use super::element::{Element, ElementType, NumberElement};
use super::slice::BufferSlice;
use crate::{
    Cx, ElementKind, Elements, Gc, JsArrayBufferView, JsObject, JsResult, JsTypedArray, NoGc,
    Shape, TypedArray, Value, errors, with_element_type,
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

/// Types of the same size and meaning share the code. The types have the same content type and different bits.
fn convert(
    target: BufferSlice<'_, u8>,
    to: ElementType,
    source: BufferSlice<'_, u8>,
    from: ElementType,
) {
    use ElementType::{Int8, Int16, Int32, Uint8, Uint8Clamped, Uint16, Uint32};
    #[inline(always)]
    fn convert_to<T: NumberElement>(
        target: BufferSlice<'_, T>,
        source: BufferSlice<'_, u8>,
        from: ElementType,
    ) {
        let from = if from == Uint8Clamped { Uint8 } else { from };
        with_element_type!(from,
            S => target.convert_ascending_from(source.cast::<S>()),
            bigint => unreachable!()
        );
    }
    let to = match to {
        Int8 => Uint8,
        Int16 => Uint16,
        Int32 => Uint32,
        other => other,
    };
    with_element_type!(to,
        T => convert_to(target.cast::<T>(), source, from),
        bigint => unreachable!()
    );
}

/// Whether the elements are all numbers, or holes that read as undefined, and there are so many.
#[inline]
fn has_only_numbers(source: &JsObject, range: &Range<usize>) -> bool {
    matches!(
        source.element_kind(),
        ElementKind::Undecided | ElementKind::Int32 | ElementKind::Double
    ) && range.end <= source.public_length() as usize
        && !source.overrides_get_own_index()
        && !source.holes_must_forward_to_prototype()
}

/// `has_only_numbers` holds, and the lengths are the same.
#[inline]
fn copy_numbers<T: NumberElement>(
    target: BufferSlice<'_, T>,
    source: &JsObject,
    range: Range<usize>,
    no_gc: &NoGc<'_>,
) {
    match source.elements(no_gc) {
        Some(Elements::Int32(values)) => {
            let hole = T::from_double(f64::NAN);
            target.fill_from(values[range].iter().map(|&value| {
                let bits = Value::bits_of(value);
                // The low half of nothing is zero, which is what an integer makes of undefined.
                if T::TYPE.is_float() && bits == 0 {
                    hole
                } else {
                    T::from_int32(bits as i32)
                }
            }))
        }
        Some(Elements::Double(numbers)) => {
            target.fill_from(numbers[range].iter().map(|&number| T::from_double(number)))
        }
        _ => unreachable!(),
    };
}

impl JsObject {
    /// Whether an element can be anything else than what the storage has.
    #[inline]
    fn overrides_get_own_index(&self) -> bool {
        self.shape().may_intercept_indexed_access()
    }
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

impl<T: NumberElement> TypedArray<T> {
    /// An array of the first `length` elements of `source`, if reading and converting them runs no code.
    #[inline]
    pub fn try_from_numbers(
        cx: &mut Cx,
        shape: Gc<Shape>,
        source: &JsObject,
        length: usize,
    ) -> JsResult<Option<Gc<TypedArray<T>>>> {
        if !has_only_numbers(source, &(0..length)) {
            return Ok(None);
        }
        Self::create_filled(cx, shape, length, |no_gc, filler| {
            copy_numbers(filler.reserve(length), source, 0..length, no_gc);
        })
        .map(Some)
    }
}

impl<T: Element> TypedArray<T> {
    /// From `offset` on, as far as the array goes at the time of each.
    pub fn set_values(&self, cx: &mut Cx, offset: usize, values: &[Value]) -> JsResult<()> {
        for (index, &value) in (offset..).zip(values) {
            self.set_value(cx, index, value)?;
        }
        Ok(())
    }

    fn set_one_by_one(
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
            let message = "Content types of source and destination typed arrays are different";
            return Err(cx.throw_type_error(message));
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
        with_element_type!(self.element_type(),
            T => {
                let (this, no_gc) = (self.typed::<T>(), cx.no_gc());
                if has_only_numbers(source, &range)
                    && let Some(target) = this.elements(&no_gc).try_slice(offset, range.len())
                {
                    copy_numbers(target, source, range, &no_gc);
                    return Ok(());
                }
                this.set_one_by_one(cx, offset, source, range)
            },
            bigint T => self.typed::<T>().set_one_by_one(cx, offset, source, range)
        )
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
