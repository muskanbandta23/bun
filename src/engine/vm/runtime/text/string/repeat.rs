use bun_alloc::AllocError;
use bun_engine_text::{CodeUnit, MAX_LENGTH, StringRef, Text};

use crate::{Cx, Gc, JsResult, JsString, RopeBuilder};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PadSide {
    Start,
    End,
}

/// Beyond these a rope is made, whose size grows with the logarithm of the count.
const MAX_COPIED_PATTERN: u32 = 8;
const MAX_COPIED_RESULT: u32 = 1024;

/// `before`, `length` units of `pattern` again and again, and `after`.
#[inline(always)]
fn fill(
    before: Text<'_>,
    pattern: Text<'_>,
    length: usize,
    after: Text<'_>,
) -> Result<StringRef, AllocError> {
    #[inline(always)]
    fn write<T: CodeUnit>(parts: [Text<'_>; 3], length: usize) -> Result<StringRef, AllocError> {
        let [before, pattern, after] = parts;
        StringRef::try_write::<T>(before.len() + length + after.len(), |cursor| {
            cursor.push_text(before);
            cursor.push_pattern(pattern, length);
            cursor.push_text(after);
        })
    }
    let is_narrow = pattern.is_latin1() || (pattern.len() == 1 && pattern.fits_latin1());
    if is_narrow && before.is_latin1() && after.is_latin1() {
        write::<u8>([before, pattern, after], length)
    } else {
        write::<u16>([before, pattern, after], length)
    }
}

impl JsString {
    /// `String.prototype.repeat`
    pub fn repeat(&self, cx: &Cx, count: u64) -> JsResult<Gc<JsString>> {
        let length = count.saturating_mul(self.len().into());
        if length > MAX_LENGTH as u64 {
            return Err(cx.throw_out_of_memory());
        }
        self.repeat_to_length(cx, length as u32)
    }

    /// The last repetition goes as far as there is room. `length` is zero if the string is empty.
    fn repeat_to_length(&self, cx: &Cx, length: u32) -> JsResult<Gc<JsString>> {
        let pattern_length = self.len();
        if length <= pattern_length {
            return self.substring(cx, 0, length);
        }
        if pattern_length == 1
            || (pattern_length <= MAX_COPIED_PATTERN && length <= MAX_COPIED_RESULT)
        {
            let filled = fill(
                Text::EMPTY,
                self.view(cx)?.text(),
                length as usize,
                Text::EMPTY,
            );
            return Ok(Self::new(cx, cx.or_out_of_memory(filled)?));
        }
        let mut builder = RopeBuilder::default();
        let (mut power, mut count) = (Gc::from(self), length / pattern_length);
        loop {
            if count & 1 != 0 {
                builder.push(cx, power)?;
            }
            count >>= 1;
            if count == 0 {
                break;
            }
            power = Self::concat(cx, power, power)?;
        }
        builder.push(cx, self.substring(cx, 0, length % pattern_length)?)?;
        Ok(builder.finish(cx))
    }

    /// StringPad
    pub fn pad(
        &self,
        cx: &Cx,
        side: PadSide,
        length: u64,
        filler: &JsString,
    ) -> JsResult<Gc<JsString>> {
        if length <= self.len().into() || filler.is_empty() {
            return Ok(self.into());
        }
        if length > MAX_LENGTH as u64 {
            return Err(cx.throw_out_of_memory());
        }
        let fill_length = length as u32 - self.len();
        if length <= MAX_COPIED_RESULT.into() && filler.len() <= MAX_COPIED_RESULT {
            let (view, filler) = (self.view(cx)?, filler.view(cx)?);
            let (before, after) = match side {
                PadSide::Start => (Text::EMPTY, view.text()),
                PadSide::End => (view.text(), Text::EMPTY),
            };
            let padded = fill(before, filler.text(), fill_length as usize, after);
            return Ok(Self::new(cx, cx.or_out_of_memory(padded)?));
        }
        let padding = filler.repeat_to_length(cx, fill_length)?;
        match side {
            PadSide::Start => Self::concat(cx, padding, self.into()),
            PadSide::End => Self::concat(cx, self.into(), padding),
        }
    }
}
