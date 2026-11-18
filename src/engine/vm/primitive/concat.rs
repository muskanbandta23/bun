use bun_alloc::AllocError;
use bun_engine_number::NumberText;
use bun_engine_text::{MAX_LENGTH, StringRef, Text};

use crate::{Cx, Gc, JsResult, JsRopeString, JsString, Value, Vm};

/// From this length on, a rope and a cell for one of its fibers take no more memory than a copy.
const MIN_ROPE_LENGTH: usize = 12;

impl JsString {
    #[inline(always)]
    pub fn try_concat(vm: &Vm, a: Gc<JsString>, b: Gc<JsString>) -> Result<Gc<Self>, AllocError> {
        let (a_length, b_length) = (a.len(), b.len());
        if a_length == 0 {
            return Ok(b);
        }
        if b_length == 0 {
            return Ok(a);
        }
        let length = a_length + b_length;
        if length as usize > MAX_LENGTH {
            return Err(AllocError);
        }
        Ok(JsRopeString::concatenation(vm, a, b, None, length))
    }

    #[inline(always)]
    pub fn concat(cx: &Cx, a: Gc<JsString>, b: Gc<JsString>) -> JsResult<Gc<JsString>> {
        cx.or_out_of_memory(Self::try_concat(cx, a, b))
    }

    #[inline]
    pub fn concat3(
        cx: &Cx,
        a: Gc<JsString>,
        b: Gc<JsString>,
        c: Gc<JsString>,
    ) -> JsResult<Gc<JsString>> {
        let (first, second) = match [a.len(), b.len(), c.len()] {
            [0, _, _] => (b, c),
            [_, 0, _] => (a, c),
            [_, _, 0] => (a, b),
            lengths => {
                let length: u64 = lengths.into_iter().map(u64::from).sum();
                if length > MAX_LENGTH as u64 {
                    return Err(cx.throw_out_of_memory());
                }
                return Ok(JsRopeString::concatenation(
                    cx,
                    a,
                    b,
                    Some(c),
                    length as u32,
                ));
            }
        };
        Self::concat(cx, first, second)
    }

    /// `a + b` for two primitives. Panics unless one of them is a string.
    #[inline(always)]
    pub fn add(cx: &Cx, a: Value, b: Value) -> JsResult<Gc<JsString>> {
        match (a.as_string(), b.as_string()) {
            (Some(a), Some(b)) => Self::concat(cx, a, b),
            _ => Self::add_mixed(cx, a, b),
        }
    }

    #[inline(never)]
    fn add_mixed(cx: &Cx, a: Value, b: Value) -> JsResult<Gc<JsString>> {
        let is_string_first = a.is_string();
        let (string, other) = if is_string_first { (a, b) } else { (b, a) };
        let string = string.cast::<JsString>();
        let try_copy = |other: Text<'_>| {
            let string = string.try_impl()?.text();
            if string.is_empty() || string.len() + other.len() >= MIN_ROPE_LENGTH {
                return None;
            }
            let mut parts = [string, other];
            if !is_string_first {
                parts.reverse();
            }
            StringRef::try_concat(&parts).ok()
        };
        if let Some(integer) = other.as_int32()
            && let Some(sum) = try_copy(NumberText::from(integer).text())
        {
            return Ok(Self::allocate(cx, sum));
        }
        let other_string = other.primitive_to_string(cx)?;
        if let Some(sum) = (other_string.try_impl()).and_then(|it| try_copy(it.text())) {
            return Ok(Self::allocate(cx, sum));
        }
        if is_string_first {
            Self::concat(cx, string, other_string)
        } else {
            Self::concat(cx, other_string, string)
        }
    }

    /// The strings of any number of primitives, one after the other.
    #[inline]
    pub fn concat_primitives(
        cx: &Cx,
        values: impl IntoIterator<Item = Value>,
    ) -> JsResult<Gc<JsString>> {
        let mut builder = RopeBuilder::default();
        for value in values {
            let string = match value.as_string() {
                Some(string) => string,
                None => value.primitive_to_string(cx)?,
            };
            builder.push(cx, string)?;
        }
        Ok(builder.finish(cx))
    }
}

/// Concatenates any number of strings.
#[derive(Default)]
pub struct RopeBuilder {
    fibers: [Option<Gc<JsString>>; 3],
    length: u32,
}

impl RopeBuilder {
    #[inline(always)]
    pub fn push(&mut self, cx: &Cx, string: Gc<JsString>) -> JsResult<()> {
        let length = self.length + string.len();
        if length == self.length {
            return Ok(());
        }
        if length as usize > MAX_LENGTH {
            return Err(cx.throw_out_of_memory());
        }
        self.fibers = match self.fibers {
            [Some(a), Some(b), c @ Some(_)] => {
                let rope = JsRopeString::concatenation(cx, a, b, c, self.length);
                [Some(rope), Some(string), None]
            }
            [a @ Some(_), b @ Some(_), None] => [a, b, Some(string)],
            [a @ Some(_), None, _] => [a, Some(string), None],
            [None, ..] => [Some(string), None, None],
        };
        self.length = length;
        Ok(())
    }

    #[inline(always)]
    pub fn finish(self, vm: &Vm) -> Gc<JsString> {
        match self.fibers {
            [Some(a), Some(b), c] => JsRopeString::concatenation(vm, a, b, c, self.length),
            [Some(only), None, _] => only,
            [None, ..] => vm.small_strings.empty(),
        }
    }
}
