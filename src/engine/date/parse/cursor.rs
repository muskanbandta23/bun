use bun_engine_text::CodeUnit;
use bun_engine_unicode::is_white_space;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Spaces {
    Ascii,
    Unicode,
}

pub(super) struct Number {
    /// Of the first nine digits after the leading zeros.
    pub(super) value: i32,
    pub(super) digits: usize,
}

impl Number {
    const SIGNIFICANT_DIGITS: usize = 9;

    /// As the digits after a decimal point.
    pub(super) fn milliseconds(&self) -> i32 {
        match self.digits.min(Number::SIGNIFICANT_DIGITS) {
            1 => self.value * 100,
            2 => self.value * 10,
            digits => self.value / 10i32.pow(digits.saturating_sub(3) as u32),
        }
    }
}

pub(super) struct Word {
    /// The first three letters in lower case, then zeros.
    pub(super) prefix: [u8; 3],
    pub(super) is_longer: bool,
}

/// A position in a text that ends at its first zero.
pub(super) struct Cursor<'a, T> {
    rest: &'a [T],
    spaces: Spaces,
}

impl<'a, T: CodeUnit> Cursor<'a, T> {
    /// What `peek` gives for anything but ASCII and white space.
    const OTHER_LETTER: u8 = 0x80;

    #[inline]
    pub(super) fn new(text: &'a [T], spaces: Spaces) -> Self {
        Cursor { rest: text, spaces }
    }

    #[inline(always)]
    fn at(&self, index: usize) -> u8 {
        match self.rest.get(index) {
            None => 0,
            Some(unit) if unit.is_ascii() => unit.to_u16() as u8,
            Some(unit) if self.spaces == Spaces::Unicode && is_white_space(unit.to_u32()) => b' ',
            Some(_) => Self::OTHER_LETTER,
        }
    }

    /// Zero at the end.
    #[inline(always)]
    pub(super) fn peek(&self) -> u8 {
        self.at(0)
    }

    #[inline(always)]
    pub(super) fn advance(&mut self) {
        self.rest = self.rest.get(1..).unwrap_or_default();
    }

    #[inline(always)]
    pub(super) fn is_at_end(&self) -> bool {
        self.peek() == 0
    }

    #[inline(always)]
    pub(super) fn skip(&mut self, ascii: u8) -> bool {
        let is_next = self.rest.first().is_some_and(|unit| unit.is(ascii));
        if is_next {
            self.advance();
        }
        is_next
    }

    /// 1 or -1.
    #[inline(always)]
    pub(super) fn sign(&mut self) -> Option<i32> {
        if self.skip(b'+') {
            return Some(1);
        }
        self.skip(b'-').then_some(-1)
    }

    /// Whether the next word is this one lower case letter, in either case.
    #[inline(always)]
    pub(super) fn is_at_letter(&self, letter: u8) -> bool {
        self.peek() | 0x20 == letter && !is_letter(self.at(1))
    }

    #[inline(always)]
    pub(super) fn skip_letter(&mut self, letter: u8) -> bool {
        let is_next = self.is_at_letter(letter);
        if is_next {
            self.advance();
        }
        is_next
    }

    /// A number of exactly `N` digits that is in the range. Nothing is consumed otherwise.
    #[inline(always)]
    pub(super) fn fixed<const N: usize>(&mut self, min: i32, max: i32) -> Option<i32> {
        let (digits, rest) = self.rest.split_first_chunk::<N>()?;
        let is_number = digits.iter().all(|unit| unit.is_ascii_digit())
            && !rest.first().is_some_and(|unit| unit.is_ascii_digit());
        let value = digits
            .iter()
            .fold(0, |value, unit| value * 10 + i32::from(unit.to_u16() & 0xf));
        if !is_number || !(min..=max).contains(&value) {
            return None;
        }
        self.rest = rest;
        Some(value)
    }

    #[inline(always)]
    pub(super) fn number(&mut self) -> Option<Number> {
        let (mut value, mut digits) = (0, 0);
        while let Some(unit) = self.rest.get(digits)
            && unit.is_ascii_digit()
        {
            if value < 10i32.pow(Number::SIGNIFICANT_DIGITS as u32 - 1) {
                value = value * 10 + i32::from(unit.to_u16() & 0xf);
            }
            digits += 1;
        }
        self.rest = &self.rest[digits..];
        (digits != 0).then_some(Number { value, digits })
    }

    #[inline(always)]
    pub(super) fn word(&mut self) -> Word {
        let (mut prefix, mut length) = ([0; 3], 0);
        while is_letter(self.at(length)) {
            if let Some(slot) = prefix.get_mut(length) {
                *slot = self.at(length) | 0x20;
            }
            length += 1;
        }
        self.rest = &self.rest[length..];
        Word {
            prefix,
            is_longer: length > prefix.len(),
        }
    }

    /// From an opening parenthesis through the one that closes it.
    pub(super) fn skip_comment(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                0 => return,
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            self.advance();
            if depth == 0 {
                return;
            }
        }
    }
}

#[inline(always)]
pub(super) fn is_letter(character: u8) -> bool {
    character >= b'A'
}
