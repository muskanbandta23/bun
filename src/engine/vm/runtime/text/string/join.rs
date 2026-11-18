//! `Array.prototype.join` and its kin, once the length and the separator are known.

use bun_alloc::AllocError;
use bun_engine_number::decimal_length;
use bun_engine_text::{CodeUnit, MAX_LENGTH, StringRef, Text};

use super::joiner::{Separator, write_number};
use crate::{
    Cx, ElementKind, Elements, Gc, JsObject, JsResult, JsString, RopeBuilder, StringJoiner, Value,
};

#[inline(always)]
fn boolean_text(value: Value) -> Text<'static> {
    Text::from_ascii(if value.is_true() { "true" } else { "false" })
}

/// The sum of the lengths of their strings and whether all are Latin-1, unless it takes a conversion to find out.
#[inline(always)]
fn measure(values: &[Option<Value>], are_holes_empty: impl Fn() -> bool) -> Option<(u64, bool)> {
    let (mut length, mut is_latin1, mut has_holes) = (0, true, false);
    for &value in values {
        let Some(value) = value else {
            has_holes = true;
            continue;
        };
        if let Some(cell) = value.as_cell() {
            let string = cell.downcast::<JsString>()?;
            length += u64::from(string.len());
            is_latin1 &= string.is_latin1();
        } else if let Some(integer) = value.as_int32() {
            length += decimal_length(integer.unsigned_abs().into()) as u64 + u64::from(integer < 0);
        } else if value.is_boolean() {
            length += boolean_text(value).len() as u64;
        } else if value.is_number() {
            return None;
        }
    }
    (!has_holes || are_holes_empty()).then_some((length, is_latin1))
}

/// They have been measured.
fn write<T: CodeUnit>(
    values: &[Option<Value>],
    separator: Text<'_>,
    length: u64,
) -> Result<StringRef, AllocError> {
    if length > MAX_LENGTH as u64 {
        return Err(AllocError);
    }
    StringRef::try_write::<T>(length as usize, |cursor| {
        let separator = Separator::new(separator);
        for (index, &value) in values.iter().enumerate() {
            if index != 0 {
                separator.write_to(cursor);
            }
            let Some(value) = value else { continue };
            if let Some(string) = value.as_string() {
                string.write_to(cursor);
            } else if let Some(integer) = value.as_int32() {
                write_number(integer, cursor);
            } else if value.is_boolean() {
                cursor.push_text(boolean_text(value));
            }
        }
    })
}

impl JsObject {
    /// With one allocation of the right size, if that can be known.
    #[inline(always)]
    fn try_join_measured(
        &self,
        cx: &mut Cx,
        count: usize,
        separator: Text<'_>,
    ) -> Option<Result<StringRef, AllocError>> {
        let no_gc = cx.no_gc();
        let (Elements::Int32(values) | Elements::Contiguous(values)) = self.elements(&no_gc)?
        else {
            return None;
        };
        let values = values.get(..count)?;
        let (length, is_latin1) = measure(values, || !self.holes_must_forward_to_prototype())?;
        let length = length + separator.len() as u64 * (count as u64 - 1);
        Some(if is_latin1 && separator.is_latin1() {
            write::<u8>(values, separator, length)
        } else {
            write::<u16>(values, separator, length)
        })
    }

    /// False, having done nothing, unless the elements are doubles without holes.
    fn try_push_doubles(&self, cx: &mut Cx, count: usize, joiner: &mut StringJoiner<'_>) -> bool {
        let no_gc = cx.no_gc();
        if let Some(Elements::Double(numbers)) = self.elements(&no_gc)
            && let Some(numbers) = numbers.get(..count)
            && !Elements::Double(numbers).has_holes()
        {
            joiner.push_numbers(numbers.iter().copied());
            return true;
        }
        false
    }

    /// The elements below `length`, with undefined and null as empty strings.
    pub fn join_elements(
        &self,
        cx: &mut Cx,
        length: u64,
        separator: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        let kind = self.element_kind();
        if length <= 1 || !kind.is_dense() || length > u64::from(self.public_length()) {
            return self.join_elements_with(cx, length, separator, |cx, it| it.to_string(cx));
        }
        if kind == ElementKind::Undecided && !self.holes_must_forward_to_prototype() {
            return separator.repeat(cx, length - 1);
        }
        let separator = separator.view(cx)?;
        if let Some(joined) = self.try_join_measured(cx, length as usize, separator.text()) {
            return Ok(JsString::new(cx, cx.or_out_of_memory(joined)?));
        }
        StringJoiner::join(cx, separator.text(), |cx, joiner| {
            if !self.try_push_doubles(cx, length as usize, joiner) {
                for index in 0..length {
                    let element = self.get_index(cx, index)?;
                    joiner.push(cx, element)?;
                }
            }
            Ok(())
        })
    }

    /// The same, with another conversion for the elements that are neither undefined nor null.
    pub fn join_elements_with(
        &self,
        cx: &mut Cx,
        length: u64,
        separator: Gc<JsString>,
        mut to_string: impl FnMut(&mut Cx, Value) -> JsResult<Gc<JsString>>,
    ) -> JsResult<Gc<JsString>> {
        let mut joined = RopeBuilder::default();
        let (mut index, mut separators) = (0, 0);
        while index < length {
            cx.check_traps()?;
            let element = self.get_index(cx, index)?;
            index += 1;
            if element.is_undefined_or_null() {
                let next = self.skip_holes(index..length);
                separators += next - index;
                index = next;
            } else {
                let string = to_string(cx, element)?;
                joined.push(cx, separator.repeat(cx, separators)?)?;
                joined.push(cx, string)?;
                separators = 0;
            }
            separators += u64::from(index < length);
            if separators.saturating_mul(separator.len().into()) > MAX_LENGTH as u64 {
                return Err(cx.throw_out_of_memory());
            }
        }
        joined.push(cx, separator.repeat(cx, separators)?)?;
        joined.finish(cx)
    }
}
