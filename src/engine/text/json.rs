use core::hint::{cold_path, select_unpredictable};
use core::mem::MaybeUninit;

use crate::block::{self, BLOCK, QUARTER, Units, singles};
use crate::scan::is_json_special;
use crate::{CodeUnit, Cursor, StringBuilder, Text, ascii, utf16};

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
    let [a, b, c, d] = [
        digit(unit >> 12),
        digit(unit >> 8),
        digit(unit >> 4),
        digit(unit),
    ];
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
        if unit.is_surrogate() {
            cold_path();
            match In::text(units).code_point_at(read) {
                ..0x1_0000 => to.push_text(Text::Latin1(&long_form(unit))),
                _ if Out::IS_LATIN1 => break,
                code_point => {
                    to.push_code_point(code_point);
                    read += 1;
                }
            }
        } else if Out::IS_LATIN1 && !unit.is_latin1() {
            break;
        } else {
            let index = usize::from(unit.min(0xff));
            let (form, length) = (
                &forms::<Out>()[index],
                usize::from(FORMS.lengths[index] & 7),
            );
            if In::IS_LATIN1 || Out::IS_LATIN1 {
                to.push_prefix(form, length);
            } else {
                let itself = Out::from_u16_truncating(unit);
                let first = select_unpredictable(unit.is_latin1(), form[0], itself);
                to.push_prefix_starting_with(first, form, length);
            }
        }
        read += 1;
    }
    read
}

/// All of a short text, which fits in `Out`.
#[inline(always)]
fn push_short<In: CodeUnit, Out: CodeUnit, const N: usize>(
    units: &[In],
    before: [u8; N],
    after: [u8; N],
    to: &mut Cursor<'_, Out>,
) {
    to.push_mapped_prefix(&before, N, Out::from);
    let mut rest = units;
    if !Out::IS_LATIN1 && !In::IS_LATIN1 && has_nothing_to_escape(units) {
        to.push_text(In::text(units));
        rest = &[];
    } else if units.len() >= SHORTEST_FOR_KERNEL && !units[0].is_trail_surrogate() {
        rest = &units[to.push_short_json_escapes(units)..];
    }
    push_characters(rest, rest.len(), to);
    to.push_mapped_prefix(&after, N, Out::from);
}

impl StringBuilder {
    /// QuoteJSONString of ECMA-262.
    #[inline(always)]
    pub fn push_json_quoted(&mut self, text: Text<'_>) {
        self.push_json_between(text, *b"\"", *b"\"");
    }

    /// `before`, the quoted key and a colon.
    #[inline(always)]
    pub fn push_json_key(&mut self, before: u8, key: Text<'_>) {
        self.push_json_between(key, [before, b'"'], *b"\":");
    }

    #[inline(always)]
    fn push_json_between<const N: usize>(
        &mut self,
        text: Text<'_>,
        before: [u8; N],
        after: [u8; N],
    ) {
        match text {
            Text::Latin1(characters) => {
                let is_done = self.push_plain::<u8, u8, N>(characters, before, after);
                self.or_else(is_done, |it| it.push_json_slow(characters, before, after));
            }
            Text::Utf16(units) => self.or_else(false, |it| it.push_json_slow(units, before, after)),
        }
    }

    /// Returns false, having pushed nothing, unless the text is short, each unit stands for itself in `Out`, and
    /// there is room.
    #[inline(always)]
    fn push_plain<In: CodeUnit, Out: CodeUnit, const N: usize>(
        &mut self,
        units: &[In],
        before: [u8; N],
        after: [u8; N],
    ) -> bool {
        let mut is_plain = false;
        if units.len() <= SHORT {
            self.write_in_room::<Out>(
                units.len() + 2 * N,
                #[inline(always)]
                |to| {
                    is_plain = if In::IS_LATIN1 && units.len() < QUARTER {
                        are_few_plain(units)
                            && to.push_between_unless(units, before, after, |_| false)
                    } else {
                        to.push_between_unless(units, before, after, needs_a_look::<In, Out>)
                    };
                },
            );
        }
        is_plain
    }

    #[inline(never)]
    fn push_json_slow<In: CodeUnit, const N: usize>(
        &mut self,
        units: &[In],
        before: [u8; N],
        after: [u8; N],
    ) -> usize {
        if self.push_plain::<In, u16, N>(units, before, after)
            || (!In::IS_LATIN1 && self.push_plain::<In, u8, N>(units, before, after))
        {
            return self.len();
        }
        self.push_json_slower(units, before, after)
    }

    #[inline(never)]
    fn push_json_slower<In: CodeUnit, const N: usize>(
        &mut self,
        units: &[In],
        before: [u8; N],
        after: [u8; N],
    ) -> usize {
        let required = units.len() + 2 * N;
        if units.len() > SHORT {
            self.push_latin1_slice(&before);
            self.push_long_json(units);
            self.push_latin1_slice(&after);
        } else if self.is_latin1() && !ascii::fits_latin1(units) {
            self.widen(required);
            self.push_json_again(units, before, after);
        } else if self.spare() < required {
            self.grow(required);
            self.push_json_again(units, before, after);
        } else if self.is_latin1() {
            self.push_short_json::<In, u8, N>(units, before, after);
        } else {
            self.push_short_json::<In, u16, N>(units, before, after);
        }
        self.len()
    }

    #[cold]
    #[inline(never)]
    fn push_json_again<In: CodeUnit, const N: usize>(
        &mut self,
        units: &[In],
        before: [u8; N],
        after: [u8; N],
    ) {
        if !self.has_overflowed() {
            self.push_json_between(In::text(units), before, after);
        }
    }

    /// The builder has units of type `Out`.
    #[inline(always)]
    fn push_short_json<In: CodeUnit, Out: CodeUnit, const N: usize>(
        &mut self,
        units: &[In],
        before: [u8; N],
        after: [u8; N],
    ) {
        let had_room = self.write_in_room::<Out>(
            room_for(units.len()) + 2 * N,
            #[inline(always)]
            |to| push_short(units, before, after, to),
        );
        if !had_room {
            self.push_through_scratch::<Out>(|to| push_short(units, before, after, to));
        }
    }

    #[inline(never)]
    fn push_long_json<In: CodeUnit>(&mut self, units: &[In]) {
        let slack = units.len() / 8 + KERNEL_SLACK;
        let mut rest = units;
        if self.is_latin1() && ascii::fits_latin1(&units[..SHORT]) {
            self.grow_with_slack(units.len(), slack);
            rest = self.push_json_fitting::<In, u8>(rest);
        }
        if !rest.is_empty() {
            if self.is_latin1() {
                self.widen(rest.len());
            }
            self.grow_with_slack(rest.len(), slack);
            self.push_json_fitting::<In, u16>(rest);
        }
    }

    /// The builder has units of type `Out` or has overflowed. Returns what is left from a character that does not fit.
    /// The less far the kernel gets, the longer the stretches that are gone through by character.
    #[inline(always)]
    fn push_json_fitting<'a, In: CodeUnit, Out: CodeUnit>(
        &mut self,
        mut rest: &'a [In],
    ) -> &'a [In] {
        let mut stretch = BLOCK;
        loop {
            let mut end = rest.len();
            if end >= SHORTEST_FOR_KERNEL {
                let mut read = 0;
                if !rest[0].is_trail_surrogate() {
                    self.grow_with_slack(rest.len(), KERNEL_SLACK);
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
            let mut read = 0;
            let had_room = self.write_in_room::<Out>(
                room_for(end),
                #[inline(always)]
                |to| read = push_characters(rest, end, to),
            );
            if !had_room {
                self.push_through_scratch::<Out>(|to| read = push_characters(rest, end, to));
            }
            rest = &rest[read..];
            if read < end || self.has_overflowed() {
                return rest;
            }
        }
    }

    /// Asks for no more room than what is written takes.
    #[inline(never)]
    fn push_through_scratch<T: CodeUnit>(&mut self, write: impl FnOnce(&mut Cursor<'_, T>)) {
        let mut scratch = [MaybeUninit::uninit(); room_for(LONGEST_STRETCH)];
        self.push_units(Cursor::fill_uninit(&mut scratch, write));
    }
}

#[inline(always)]
fn needs_a_look<In: CodeUnit, Out: CodeUnit>(unit: In) -> bool {
    let is_doubtful = if Out::IS_LATIN1 {
        !unit.is_latin1()
    } else {
        unit.is_surrogate()
    };
    is_json_special(unit.to_u16()) | is_doubtful
}

/// For up to three Latin-1 characters.
#[inline(always)]
fn are_few_plain<T: CodeUnit>(units: &[T]) -> bool {
    let length = |at: usize| FORMS.lengths[usize::from(units[at].to_u16() as u8)];
    units.is_empty() || {
        let [a, b, c] = singles(units.len());
        length(a) | length(b) | length(c) == 1
    }
}

#[inline(always)]
fn has_nothing_to_escape<T: CodeUnit>(units: &[T]) -> bool {
    !block::any(Units(units, |unit: T| is_json_special(unit.to_u16())))
        && u16::units(T::text(units)).is_none_or(utf16::has_only_pairs)
}
