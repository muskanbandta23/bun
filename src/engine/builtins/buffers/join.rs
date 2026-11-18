use bun_engine_number::{
    NUMBER_TEXT_CAPACITY, write_f64, write_i32, write_i64, write_u32, write_u64,
};
use bun_engine_text::{CodeUnit, Cursor, MAX_LENGTH, StringBuilder, Text};
use bun_engine_vm::{BufferSlice, ClampedU8, Element, Float16, with_element_type};

use super::prototype::valid_this;
use crate::prelude::*;

type Digits = [u8; NUMBER_TEXT_CAPACITY];

trait Write: Element {
    /// Returns the length of the text.
    fn write(self, out: &mut Digits) -> usize;
}

macro_rules! writers {
    ($($write:ident: $($type:ty)*;)*) => {$($(
        impl Write for $type {
            #[inline(always)]
            fn write(self, out: &mut Digits) -> usize {
                $write(self.into(), out)
            }
        }
    )*)*};
}

writers! {
    write_i32: i8 i16 i32;
    write_u32: u8 u16 u32;
    write_i64: i64;
    write_u64: u64;
    write_f64: Float16 f32 f64;
}

impl Write for ClampedU8 {
    #[inline(always)]
    fn write(self, out: &mut Digits) -> usize {
        self.0.write(out)
    }
}

/// Each after the separator.
#[inline(always)]
fn push_batch<T: Write, U: CodeUnit>(
    elements: BufferSlice<'_, T>,
    separator: &[U],
    to: &mut Cursor<'_, U>,
) {
    let mut digits = Digits::default();
    for element in elements.iter() {
        match separator {
            &[unit] => to.push(unit),
            _ => to.push_slice(separator),
        }
        let length = element.write(&mut digits);
        to.push_prefix(&digits.map(U::from), length);
    }
}

fn push_elements<T: Write>(
    elements: BufferSlice<'_, T>,
    separator: Text<'_>,
    out: &mut StringBuilder,
) {
    const BATCH: usize = 64;
    let Some(first) = elements.get(0) else { return };
    out.write_latin1(NUMBER_TEXT_CAPACITY, |to| {
        let mut digits = Digits::default();
        let length = first.write(&mut digits);
        to.push_prefix(&digits, length);
    });
    for start in (1..elements.len()).step_by(BATCH) {
        let batch = elements.clamped(start..start + BATCH);
        let room = batch.len() * (separator.len() + NUMBER_TEXT_CAPACITY);
        match separator {
            Text::Latin1(it) => out.write_latin1(room, |to| push_batch(batch, it, to)),
            Text::Utf16(it) => out.write_utf16(room, |to| push_batch(batch, it, to)),
        }
        if out.has_overflowed() {
            return;
        }
        if start == 1 {
            // The rest is likely to be like the beginning.
            let expected = out.len() * elements.len() / (1 + batch.len());
            out.reserve((expected + expected / 16 + room).min(MAX_LENGTH) - out.len());
        }
    }
}

#[native_abi]
pub(super) fn join(cx: &mut Cx, frame: &CallFrame) -> JsResult<Value> {
    let (this, length) = valid_this(cx, frame)?;
    let view = match frame.argument(0) {
        Value::UNDEFINED => None,
        separator => Some(separator.to_string_view(cx)?),
    };
    let separator = view.as_ref().map_or(Text::Latin1(b","), |it| it.text());
    let present = length.min(this.length());
    if present + length.saturating_sub(1).saturating_mul(separator.len()) > MAX_LENGTH {
        return Err(cx.throw_out_of_memory());
    }
    if length == 1
        && let Some(element) = this.get_element(cx, 0)
    {
        return element.primitive_to_string(cx).map(Into::into);
    }
    let text = StringBuilder::on_stack::<4096, _>(|out| {
        with_element_type!(this.element_type(), T => {
            let no_gc = cx.no_gc();
            push_elements(this.typed::<T>().elements(&no_gc).slice(0..present), separator, out);
        });
        for _ in present.max(1)..length {
            out.push_text(separator);
        }
        out.take()
    });
    Ok(JsString::new(cx, cx.or_out_of_memory(text)?).into())
}
