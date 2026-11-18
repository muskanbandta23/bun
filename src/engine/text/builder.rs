use core::mem::MaybeUninit;
use core::ptr::NonNull;

use bun_alloc::AllocError;

use crate::{CodeUnit, MAX_LENGTH, StringImpl, StringRef, Text, with_text};

/// A string that grows in the allocation it ends up in. It is Latin-1 until a wider character arrives. Once it has
/// run out of memory or of length it takes nothing more, and `finish` fails.
pub struct StringBuilder {
    /// Its length is the capacity.
    buffer: StringRef,
    length: usize,
    capacity: usize,
    is_latin1: bool,
    has_overflowed: bool,
}

impl Default for StringBuilder {
    fn default() -> StringBuilder {
        StringBuilder::new()
    }
}

impl StringBuilder {
    const MINIMUM_CAPACITY: usize = 28;

    #[inline]
    pub const fn new() -> StringBuilder {
        StringBuilder {
            buffer: StringRef::empty(),
            length: 0,
            capacity: 0,
            is_latin1: true,
            has_overflowed: false,
        }
    }

    pub fn with_capacity(capacity: usize) -> StringBuilder {
        let mut builder = StringBuilder::new();
        builder.reserve(capacity);
        builder
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.length
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    #[inline]
    pub fn is_latin1(&self) -> bool {
        self.is_latin1
    }

    #[inline]
    pub fn has_overflowed(&self) -> bool {
        self.has_overflowed
    }

    pub fn text(&self) -> Text<'_> {
        if self.capacity == 0 {
            return Text::EMPTY;
        }
        // SAFETY: the first `length` units are written, in the width that the flag names.
        unsafe {
            let units = self.buffer.as_ptr().byte_add(StringImpl::TAIL);
            if self.is_latin1 {
                Text::Latin1(core::slice::from_raw_parts(
                    units.cast().as_ptr(),
                    self.length,
                ))
            } else {
                Text::Utf16(core::slice::from_raw_parts(
                    units.cast().as_ptr(),
                    self.length,
                ))
            }
        }
    }

    pub fn clear(&mut self) {
        *self = StringBuilder::new();
    }

    pub fn truncate(&mut self, length: usize) {
        self.length = self.length.min(length);
    }

    pub fn reserve(&mut self, additional: usize) {
        if additional > self.capacity - self.length {
            let capacity = self.length.saturating_add(additional);
            if self.is_latin1 {
                self.resize::<u8>(capacity);
            } else {
                self.resize::<u16>(capacity);
            }
        }
    }

    #[cold]
    fn overflow(&mut self) {
        self.clear();
        self.has_overflowed = true;
    }

    fn resize<T: CodeUnit>(&mut self, capacity: usize) {
        debug_assert!(T::IS_LATIN1 == self.is_latin1);
        if self.has_overflowed {
            return;
        }
        let resized = if self.capacity == 0 {
            StringImpl::allocate::<T>(capacity)
        } else {
            // SAFETY: allocated for `T`, and the builder holds the only reference.
            unsafe { StringImpl::reallocate::<T>(self.buffer.as_ptr(), capacity) }
        };
        match resized {
            Ok(buffer) => self.adopt(buffer, capacity),
            Err(AllocError) => self.overflow(),
        }
    }

    /// The old buffer has moved into `buffer` or is the empty string.
    fn adopt(&mut self, buffer: NonNull<StringImpl>, capacity: usize) {
        // SAFETY: the reference that the allocation was made with.
        let old = core::mem::replace(&mut self.buffer, unsafe { StringRef::from_raw(buffer) });
        old.into_raw();
        self.capacity = capacity;
    }

    fn grown_capacity(&self, additional: usize) -> usize {
        let required = self.length.saturating_add(additional);
        required
            .max((self.capacity * 2).min(MAX_LENGTH))
            .max(Self::MINIMUM_CAPACITY)
    }

    #[cold]
    #[inline(never)]
    fn grow<T: CodeUnit>(&mut self, additional: usize) -> bool {
        self.resize::<T>(self.grown_capacity(additional));
        !self.has_overflowed
    }

    #[cold]
    #[inline(never)]
    fn widen(&mut self, additional: usize) {
        if self.has_overflowed {
            return;
        }
        let capacity = if additional > self.capacity - self.length {
            self.grown_capacity(additional)
        } else {
            self.capacity
        };
        let Ok(wide) = StringImpl::allocate::<u16>(capacity) else {
            return self.overflow();
        };
        // SAFETY: a new allocation for `u16` that nothing else has.
        let units = unsafe { StringImpl::tail::<u16>(wide) };
        for (target, unit) in units.iter_mut().zip(self.text().code_units()) {
            target.write(unit);
        }
        // SAFETY: the reference that the allocation was made with.
        self.buffer = unsafe { StringRef::from_raw(wide) };
        self.capacity = capacity;
        self.is_latin1 = false;
    }

    /// The next `count` units, all of which the caller writes. Empty after an overflow.
    #[inline(always)]
    fn extend<T: CodeUnit>(&mut self, count: usize) -> &mut [MaybeUninit<T>] {
        debug_assert!(T::IS_LATIN1 == self.is_latin1);
        if count > self.capacity - self.length && !self.grow::<T>(count) {
            return &mut [];
        }
        let start = self.length;
        self.length += count;
        // SAFETY: inside the capacity, which is in units of `T`, and only the builder has the buffer.
        unsafe {
            let units = self
                .buffer
                .as_ptr()
                .byte_add(StringImpl::TAIL)
                .cast::<MaybeUninit<T>>();
            core::slice::from_raw_parts_mut(units.add(start).as_ptr(), count)
        }
    }

    #[inline(always)]
    fn extend_mapped<S: Copy, T: CodeUnit>(&mut self, source: &[S], map: impl Fn(S) -> T) {
        for (target, &unit) in self.extend(source.len()).iter_mut().zip(source) {
            target.write(map(unit));
        }
    }

    #[inline(always)]
    fn extend_from_slice<T: CodeUnit>(&mut self, units: &[T]) {
        let target = self.extend::<T>(units.len());
        let count = target.len();
        target.write_copy_of_slice(&units[..count]);
    }

    #[inline]
    pub fn push_latin1(&mut self, character: u8) {
        if self.is_latin1 {
            self.extend_mapped(&[character], |character| character);
        } else {
            self.extend_mapped(&[character], u16::from);
        }
    }

    #[inline]
    pub fn push_unit(&mut self, unit: u16) {
        if self.is_latin1 {
            if let Ok(character) = u8::try_from(unit) {
                return self.extend_mapped(&[character], |character| character);
            }
            self.widen(1);
            if self.has_overflowed {
                return;
            }
        }
        self.extend_mapped(&[unit], |unit| unit);
    }

    /// `code_point` is at most 0x10FFFF.
    #[inline]
    pub fn push_code_point(&mut self, code_point: u32) {
        match u16::try_from(code_point) {
            Ok(unit) => self.push_unit(unit),
            Err(_) => {
                let offset = code_point - 0x10000;
                self.push_utf16(&[
                    0xd800 | (offset >> 10) as u16,
                    0xdc00 | (offset & 0x3ff) as u16,
                ]);
            }
        }
    }

    #[inline]
    pub fn push_latin1_slice(&mut self, characters: &[u8]) {
        if self.is_latin1 {
            self.extend_from_slice(characters);
        } else {
            self.extend_mapped(characters, u16::from);
        }
    }

    #[inline]
    pub fn push_ascii(&mut self, ascii: &str) {
        debug_assert!(ascii.is_ascii());
        self.push_latin1_slice(ascii.as_bytes());
    }

    pub fn push_utf16(&mut self, units: &[u16]) {
        if self.is_latin1 {
            if Text::Utf16(units).fits_latin1() {
                return self.extend_mapped(units, |unit| unit as u8);
            }
            self.widen(units.len());
            if self.has_overflowed {
                return;
            }
        }
        self.extend_from_slice(units);
    }

    #[inline]
    pub fn push_text(&mut self, text: Text<'_>) {
        match text {
            Text::Latin1(characters) => self.push_latin1_slice(characters),
            Text::Utf16(units) => self.push_utf16(units),
        }
    }

    #[inline]
    pub fn push_units<T: CodeUnit>(&mut self, units: &[T]) {
        self.push_text(T::text(units));
    }

    pub fn push_repeated(&mut self, character: u8, count: usize) {
        if self.is_latin1 {
            self.extend(count).write_filled(character);
        } else {
            self.extend(count).write_filled(u16::from(character));
        }
    }

    /// QuoteJSONString of ECMA-262.
    pub fn push_json_quoted(&mut self, text: Text<'_>) {
        self.reserve(text.len().saturating_add(2));
        self.push_latin1(b'"');
        with_text!(text, |units| self.push_json_escaped(units));
        self.push_latin1(b'"');
    }

    fn push_json_escaped<T: CodeUnit>(&mut self, units: &[T]) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let needs_escape = |unit: T| {
            let unit = unit.to_u16();
            unit < 0x20 || unit == 0x22 || unit == 0x5c || unit & 0xf800 == 0xd800
        };
        let mut rest = units;
        while let Some(index) = rest.iter().position(|&unit| needs_escape(unit)) {
            self.push_units(&rest[..index]);
            let unit = rest[index].to_u16();
            let next = rest.get(index + 1).map(|next| next.to_u16());
            rest = &rest[index + 1..];
            let short = match unit {
                0x08 => b'b',
                0x09 => b't',
                0x0a => b'n',
                0x0c => b'f',
                0x0d => b'r',
                0x22 | 0x5c => unit as u8,
                0xd800..=0xdbff if next.is_some_and(|next| next & 0xfc00 == 0xdc00) => {
                    self.push_utf16(&[unit, next.unwrap_or_default()]);
                    rest = &rest[1..];
                    continue;
                }
                _ => {
                    let digit = |shift: u32| HEX[usize::from(unit >> shift & 0xf)];
                    self.push_latin1_slice(&[b'\\', b'u', digit(12), digit(8), digit(4), digit(0)]);
                    continue;
                }
            };
            self.push_latin1_slice(&[b'\\', short]);
        }
        self.push_units(rest);
    }

    pub fn finish(mut self) -> Result<StringRef, AllocError> {
        if self.has_overflowed {
            return Err(AllocError);
        }
        match self.text() {
            Text::Latin1([]) | Text::Utf16([]) => return Ok(StringRef::empty()),
            Text::Latin1(&[character]) => return Ok(StringRef::single_character(character)),
            _ => {}
        }
        if self.length != self.capacity {
            if self.is_latin1 {
                self.resize::<u8>(self.length);
            } else {
                self.resize::<u16>(self.length);
            }
            if self.has_overflowed {
                return Err(AllocError);
            }
        }
        Ok(self.buffer)
    }
}

impl core::fmt::Write for StringBuilder {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        if text.is_ascii() {
            self.push_ascii(text);
        } else {
            text.chars()
                .for_each(|character| self.push_code_point(u32::from(character)));
        }
        Ok(())
    }
}
