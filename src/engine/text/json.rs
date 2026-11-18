use crate::block::{self, BLOCK, QUARTER, Units, singles};
use crate::scan::is_json_special;
use crate::{CodeUnit, Cursor, StringBuilder, Text, utf16};

/// Up to here a text is copied in the hope that it has nothing to escape.
const SHORT: usize = 64;
/// Up to here the kernel does not make up for the call.
const SHORTEST_FOR_KERNEL: usize = 16;
/// With so much more room than there are units left, the kernel keeps to its widest steps.
const KERNEL_SLACK: usize = 128;
const LONGEST_ESCAPE: usize = 6;

const LONGEST_STRETCH: usize = 256;

/// What stands for a character, padded.
type Form<T> = [T; 8];

struct Forms {
    latin1: [Form<u8>; 256],
    utf16: [Form<u16>; 256],
    lengths: [u8; 256],
}

static FORMS: Forms = {
    let mut forms = Forms {
        latin1: [[0; 8]; 256],
        utf16: [[0; 8]; 256],
        lengths: [0; 256],
    };
    let mut character = 0;
    while character < 256 {
        let short = match character as u8 {
            0x08 => b'b',
            0x09 => b't',
            0x0a => b'n',
            0x0c => b'f',
            0x0d => b'r',
            other => other,
        };
        let (form, length): (&[u8], _) = match character as u8 {
            _ if short < 0x20 => (&long_form(character as u16), LONGEST_ESCAPE),
            ..0x20 | b'"' | b'\\' => (&[b'\\', short], 2),
            _ => (&[short], 1),
        };
        forms.lengths[character] = length as u8;
        let mut index = 0;
        while index < length {
            forms.latin1[character][index] = form[index];
            forms.utf16[character][index] = form[index] as u16;
            index += 1;
        }
        character += 1;
    }
    forms
};

const fn long_form(unit: u16) -> [u8; LONGEST_ESCAPE] {
    const fn digit(value: u16) -> u8 {
        b"0123456789abcdef"[(value & 15) as usize]
    }
    let [a, b, c, d] = [digit(unit >> 12), digit(unit >> 8), digit(unit >> 4), digit(unit)];
    [b'\\', b'u', a, b, c, d]
}

#[inline(always)]
fn forms<T: CodeUnit>() -> &'static [Form<T>] {
    let units = if T::IS_LATIN1 {
        Text::Latin1(FORMS.latin1.as_flattened())
    } else {
        Text::Utf16(FORMS.utf16.as_flattened())
    };
    T::units(units).unwrap_or_default().as_chunks().0
}

/// The room that `push_characters` needs for so many units.
const fn room_for(count: usize) -> usize {
    count * LONGEST_ESCAPE + size_of::<Form<u8>>()
}

/// Goes on up to the character that starts at `end` or later, or that does not fit in `Out`. Returns how many units
/// it has read.
#[inline(always)]
fn push_characters<In: CodeUnit, Out: CodeUnit>(
    units: &[In],
    end: usize,
    to: &mut Cursor<'_, Out>,
) -> usize {
    assert!(end <= units.len());
    let mut read = 0;
    while read < end {
        let unit = units[read].to_u16();
        if let Some(form) = forms::<Out>().get(usize::from(unit)) {
            to.push_prefix(form, usize::from(FORMS.lengths[usize::from(unit)] & 7));
        } else if !unit.is_surrogate() {
            if Out::IS_LATIN1 {
                break;
            }
            to.push(Out::from_u16_truncating(unit));
        } else if let code_point @ 0x1_0000.. = In::text(units).code_point_at(read) {
            if Out::IS_LATIN1 {
                break;
            }
            to.push_code_point(code_point);
            read += 1;
        } else {
            to.push_text(Text::Latin1(&long_form(unit)));
        }
        read += 1;
    }
    read
}

impl StringBuilder {
    /// QuoteJSONString of ECMA-262.
    #[inline(always)]
    pub fn push_json_quoted(&mut self, text: Text<'_>) {
        match text {
            Text::Latin1(characters) => {
                let is_done = self.push_plain_quoted::<u8, u8>(characters);
                self.or_else(is_done, |it| it.push_json_quoted_slow(characters));
            }
            Text::Utf16(units) => self.or_else(false, |it| it.push_json_quoted_slow(units)),
        }
    }

    /// Returns false, having pushed nothing, unless the text is short, each unit stands for itself in `Out`, and
    /// there is room.
    #[inline(always)]
    fn push_plain_quoted<In: CodeUnit, Out: CodeUnit>(&mut self, units: &[In]) -> bool {
        let mut is_plain = false;
        if let (Some(characters), 1..QUARTER) = (u8::units(In::text(units)), units.len()) {
            let [a, b, c] = singles(units.len());
            let length = |at: usize| FORMS.lengths[usize::from(characters[at])];
            is_plain = length(a) | length(b) | length(c) == 1
                && self.write_in_room::<Out>(
                    units.len() + 2,
                    #[inline(always)]
                    |to| _ = to.push_quoted_unless(units, |_| false),
                );
        } else if units.len() <= SHORT {
            self.write_in_room::<Out>(
                units.len() + 2,
                #[inline(always)]
                |to| {
                    is_plain = to.push_quoted_unless(units, |unit| {
                        let needs_a_look = if Out::IS_LATIN1 {
                            !unit.is_latin1()
                        } else {
                            unit.is_surrogate()
                        };
                        is_json_special(unit.to_u16()) | needs_a_look
                    });
                },
            );
        }
        is_plain
    }

    #[inline(never)]
    fn push_json_quoted_slow<In: CodeUnit>(&mut self, units: &[In]) -> usize {
        if self.push_plain_quoted::<In, u16>(units) {
            return self.len();
        }
        self.push_json_quoted_slower(units)
    }

    #[inline(never)]
    fn push_json_quoted_slower<In: CodeUnit>(&mut self, units: &[In]) -> usize {
        let required = units.len() + 2;
        if self.is_latin1() {
            if !In::IS_LATIN1 || !self.has_room::<u8>(required) {
                self.grow(required);
                if self.push_plain_quoted::<In, u8>(units) {
                    return self.len();
                }
            }
            self.push_latin1(b'"');
            match self.push_json_escaped::<In, u8>(units) {
                [] => self.push_latin1(b'"'),
                rest => {
                    self.widen(rest.len() + 1);
                    self.push_wide_json_rest(rest);
                }
            }
        } else {
            self.grow(required);
            self.push_latin1(b'"');
            self.push_wide_json_rest(units);
        }
        self.len()
    }

    #[inline(always)]
    fn push_wide_json_rest<In: CodeUnit>(&mut self, units: &[In]) {
        if is_short_and_plain(units) {
            self.push_units(units);
        } else {
            self.push_json_escaped::<In, u16>(units);
        }
        self.push_latin1(b'"');
    }

    /// The builder has units of type `Out` or has overflowed. Returns what is left from a character that does not fit.
    /// The less far the kernel gets, the longer the stretches that are gone through by character.
    #[inline(always)]
    fn push_json_escaped<'a, In: CodeUnit, Out: CodeUnit>(&mut self, mut rest: &'a [In]) -> &'a [In] {
        let mut stretch = BLOCK;
        loop {
            let mut end = rest.len();
            if end >= SHORTEST_FOR_KERNEL {
                let mut read = 0;
                if !rest[0].is_trail_surrogate() {
                    self.grow_with_slack(rest.len(), rest.len() / 8 + KERNEL_SLACK);
                    self.write_in_spare::<Out>(|to| read = to.push_short_json_escapes(rest));
                }
                rest = &rest[read..];
                stretch = if read < SHORT {
                    LONGEST_STRETCH.min(2 * stretch)
                } else {
                    BLOCK
                };
                end = rest.len().min(stretch);
            }
            if end == 0 {
                return rest;
            }
            self.grow_with_slack(end, room_for(end) - end);
            let mut read = 0;
            let had_room = self.write_in_room::<Out>(
                room_for(end),
                #[inline(always)]
                |to| read = push_characters(rest, end, to),
            );
            if !had_room {
                end = end.min(BLOCK);
                read = self.push_last_json_characters::<In, Out>(rest, end);
            }
            rest = &rest[read..];
            if read < end || self.has_overflowed() {
                return rest;
            }
        }
    }

    /// For a string that is nearly as long as it can be.
    #[cold]
    #[inline(never)]
    fn push_last_json_characters<In: CodeUnit, Out: CodeUnit>(
        &mut self,
        units: &[In],
        end: usize,
    ) -> usize {
        let (mut scratch, mut read) = ([Out::default(); room_for(BLOCK)], 0);
        let written = Cursor::fill(&mut scratch, |to| read = push_characters(units, end, to));
        self.push_units(&scratch[..written]);
        read
    }
}

#[inline(always)]
fn is_short_and_plain<T: CodeUnit>(units: &[T]) -> bool {
    units.len() <= SHORT
        && !block::any(Units(units, |unit: T| is_json_special(unit.to_u16())))
        && u16::units(T::text(units)).is_none_or(utf16::has_only_pairs)
}
