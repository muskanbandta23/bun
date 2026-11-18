use crate::scan::is_json_special;
use crate::{CodeUnit, Cursor, StringBuilder, Text, with_text};

/// Up to here a text is copied in the hope that it has nothing to escape.
const SHORT: usize = 64;
/// What the kernel may leave for want of room or because of a unit that it does not handle.
const KERNEL_BLOCK: usize = 16;
/// With so much more room than there are units left, the kernel keeps to its widest steps.
const KERNEL_SLACK: usize = 128;

/// Returns false, having pushed nothing, if a unit does not stand for itself in `Out`.
#[inline(always)]
fn push_plain<In: CodeUnit, Out: CodeUnit>(cursor: &mut Cursor<'_, Out>, units: &[In]) -> bool {
    cursor.push_mapped_unless(
        units,
        |unit| Out::from_u16_truncating(unit.to_u16()),
        |unit| {
            let needs_a_look = if Out::IS_LATIN1 {
                !unit.is_latin1()
            } else {
                unit.is_surrogate()
            };
            is_json_special(unit.to_u16()) | needs_a_look
        },
    )
}

impl StringBuilder {
    /// QuoteJSONString of ECMA-262.
    #[inline]
    pub fn push_json_quoted(&mut self, text: Text<'_>) {
        if let Text::Latin1(characters) = text
            && self.push_json_quoted_plain::<u8, u8>(characters)
        {
            return;
        }
        self.push_json_quoted_slow(text);
    }

    /// Returns false, having pushed nothing, unless the text is short, has nothing to escape, and fits as `Out`.
    #[inline(always)]
    fn push_json_quoted_plain<In: CodeUnit, Out: CodeUnit>(&mut self, units: &[In]) -> bool {
        if units.len() > SHORT || !self.has_room::<Out>(units.len() + 2) {
            return false;
        }
        let (start, mut is_plain) = (self.len(), false);
        self.write_in_spare::<Out>(|cursor| {
            cursor.push(Out::from(b'"'));
            is_plain = push_plain(cursor, units);
            if is_plain {
                cursor.push(Out::from(b'"'));
            }
        });
        if !is_plain {
            self.truncate(start);
        }
        is_plain
    }

    #[inline(never)]
    fn push_json_quoted_slow(&mut self, text: Text<'_>) {
        let is_done = match text {
            Text::Latin1(characters) => self.push_json_quoted_plain::<u8, u16>(characters),
            Text::Utf16(units) => {
                self.push_json_quoted_plain::<u16, u16>(units)
                    || self.push_json_quoted_plain::<u16, u8>(units)
            }
        };
        if !is_done {
            self.push_latin1(b'"');
            with_text!(text, |units| self.push_json_escaped(units));
            self.push_latin1(b'"');
        }
    }

    fn push_json_escaped<T: CodeUnit>(&mut self, mut rest: &[T]) {
        while !rest.is_empty() && !self.has_overflowed() {
            self.grow_with_slack(rest.len(), rest.len() / 8 + KERNEL_SLACK);
            let mut read = 0;
            if !rest[0].is_trail_surrogate() {
                if self.is_latin1() {
                    self.write_in_spare::<u8>(|it| read = it.push_short_json_escapes(rest));
                } else {
                    self.write_in_spare::<u16>(|it| read = it.push_short_json_escapes(rest));
                }
            }
            let end = rest.len().min(read + KERNEL_BLOCK);
            while read < end {
                read += self.push_json_character(&rest[read..]);
            }
            rest = &rest[read..];
        }
    }

    /// Returns how many of `units`, which are not empty, the first character takes.
    #[inline(always)]
    fn push_json_character<T: CodeUnit>(&mut self, units: &[T]) -> usize {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let unit = units[0].to_u16();
        if !is_json_special(unit) && !unit.is_surrogate() {
            self.push_unit(unit);
            return 1;
        }
        let short = match unit {
            0x08 => b'b',
            0x09 => b't',
            0x0a => b'n',
            0x0c => b'f',
            0x0d => b'r',
            0x22 | 0x5c => unit as u8,
            0xd800..=0xdbff if units.get(1).is_some_and(|next| next.is_trail_surrogate()) => {
                self.push_utf16(&[unit, units[1].to_u16()]);
                return 2;
            }
            _ => {
                let digit = |shift: u32| HEX[usize::from(unit >> shift & 0xf)];
                self.push_latin1_slice(&[b'\\', b'u', digit(12), digit(8), digit(4), digit(0)]);
                return 1;
            }
        };
        self.push_latin1_slice(&[b'\\', short]);
        1
    }
}
