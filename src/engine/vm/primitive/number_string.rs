use bun_engine_number::{Radix, integer_to_radix, parse_int, with_radix_text};
use bun_engine_text::{CodeUnit, Text, with_text};

use crate::{Cx, Gc, JsResult, JsString, NoGc, Vm};

impl JsString {
    /// Number::toString
    pub fn from_number_in_radix(vm: &Vm, number: f64, radix: Radix) -> Gc<JsString> {
        if radix == Radix::DECIMAL {
            return Self::from_number(vm, number);
        }
        with_radix_text(number, radix, |text| {
            Self::from_text(vm, Text::Latin1(text))
        })
    }

    /// The same, for an integer of at most 53 bits.
    pub fn from_integer_in_radix(vm: &Vm, number: i64, radix: Radix) -> Gc<JsString> {
        if radix != Radix::DECIMAL {
            let mut buffer = [0; _];
            let text = integer_to_radix(number, radix, &mut buffer);
            return Self::from_text(vm, Text::Latin1(text));
        }
        match i32::try_from(number) {
            Ok(number) => Self::from_int32(vm, number),
            Err(_) => Self::from_number(vm, number as f64),
        }
    }

    /// What `parseInt` makes of it. `radix` is the result of ToInt32.
    #[inline]
    pub fn parse_int(&self, cx: &mut Cx, radix: i32) -> JsResult<f64> {
        if let Some(view) = self.try_view() {
            return Ok(parse_int(view.text(), radix));
        }
        if self.len() as usize > MAX_COPIED_LENGTH {
            return Ok(parse_int(self.view(cx)?.text(), radix));
        }
        Ok(self.parse_int_of_short_rope(&cx.no_gc(), radix))
    }

    /// The pieces of a number are rarely used again, so they are not put together for good.
    fn parse_int_of_short_rope(&self, no_gc: &NoGc<'_>, radix: i32) -> f64 {
        let mut buffer = [0u16; MAX_COPIED_LENGTH];
        let mut start = MAX_COPIED_LENGTH;
        self.for_each_piece_reversed(no_gc, |piece| {
            start -= piece.len();
            with_text!(piece, |units| {
                for (target, unit) in buffer[start..].iter_mut().zip(units) {
                    *target = unit.to_u16();
                }
            });
        });
        parse_int(Text::Utf16(&buffer[start..]), radix)
    }
}

const MAX_COPIED_LENGTH: usize = 64;
