use bun_engine_text::CodeUnit;
use bun_engine_unicode::{MAX_CODE_POINT, Property};

use super::quantifier::Repeated;
use super::{
    Assertion, ClassEscape, ClassKind, Parser, Sink, is_lead_surrogate, is_trail_surrogate,
    join_surrogates,
};
use crate::Error;

#[derive(Clone, Copy)]
pub(super) enum ClassAtom {
    Character(u32),
    Class(ClassEscape),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Context {
    Atom,
    Class,
    ClassSet,
}

const fn ascii_mask(characters: &[u8]) -> u128 {
    let (mut mask, mut index) = (0, 0);
    while index < characters.len() {
        mask |= 1 << characters[index];
        index += 1;
    }
    mask
}

#[inline]
pub(super) const fn is_in(mask: u128, c: u32) -> bool {
    c < 128 && mask >> c & 1 != 0
}

const SYNTAX_CHARACTERS_AND_SOLIDUS: u128 = ascii_mask(b"^$\\.*+?()[]{}|/");
const CLASS_SET_RESERVED_PUNCTUATORS: u128 = ascii_mask(b"&-!#%,:;<=>@`~");
pub(super) const CLASS_SET_SYNTAX_CHARACTERS: u128 = ascii_mask(b"()[]{}/-\\|");
pub(super) const CLASS_SET_RESERVED_IF_DOUBLED: u128 = ascii_mask(b"&!#$%*+,.:;<=>?@^`~");

impl<C: CodeUnit, S: Sink> Parser<'_, C, S> {
    /// After the backslash.
    #[inline]
    pub(super) fn atom_escape(&mut self) -> Result<(), Error> {
        match self.peek() {
            letter @ (b'b' | b'B') => {
                self.at += 1;
                let negated = letter == b'B';
                return self.sink.assertion(Assertion::WordBoundary { negated });
            }
            b'1'..=b'9' if self.back_reference()? => {}
            b'k' if self.named_reference()? => {}
            _ => match self.escape(Context::Atom)? {
                ClassAtom::Character(c) => self.sink.character(c)?,
                ClassAtom::Class(escape) if escape.has_strings() => {
                    self.sink.class_member(escape)?;
                    let quantifier = self.quantifier(Repeated::Atom)?;
                    return self.sink.close_class(false, quantifier);
                }
                ClassAtom::Class(escape) => self.sink.class_escape(escape)?,
            },
        }
        self.quantify()
    }

    /// After the backslash.
    #[inline]
    pub(super) fn escape(&mut self, context: Context) -> Result<ClassAtom, Error> {
        let letter = self.peek();
        let kind = match letter | 0x20 {
            b'd' => ClassKind::Digit,
            b's' => ClassKind::Space,
            b'w' => ClassKind::Word,
            b'p' if self.is_unicode => return self.property().map(ClassAtom::Class),
            _ => return self.character_escape(context).map(ClassAtom::Character),
        };
        self.at += 1;
        let negated = letter.is_ascii_uppercase();
        Ok(ClassAtom::Class(ClassEscape { kind, negated }))
    }

    /// After the backslash.
    #[inline]
    pub(super) fn character_escape(&mut self, context: Context) -> Result<u32, Error> {
        let unit = self.next_unit().ok_or(Error::EscapeAtEnd)?;
        if is_in(SYNTAX_CHARACTERS_AND_SOLIDUS, unit) {
            return Ok(unit);
        }
        let value = match unit.min(0xff) as u8 {
            b'b' if context != Context::Atom => Some(0x08),
            b't' => Some(0x09),
            b'n' => Some(0x0a),
            b'v' => Some(0x0b),
            b'f' => Some(0x0c),
            b'r' => Some(0x0d),
            b'0' if !self.peek().is_ascii_digit() => Some(0),
            b'0'..=b'9' if self.is_unicode => return Err(Error::InvalidOctalEscape),
            b'0'..=b'7' => Some(self.octal(unit - u32::from(b'0'))),
            b'c' => return self.control_escape(context),
            b'x' => self.hexadecimal(2),
            b'u' if self.is_unicode => {
                self.at -= 1;
                return self.strict_unicode_escape();
            }
            b'u' => self.hexadecimal(4),
            b'k' => {
                self.has_escaped_k = true;
                None
            }
            b'-' if context == Context::Class => Some(unit),
            _ if context == Context::ClassSet && is_in(CLASS_SET_RESERVED_PUNCTUATORS, unit) => {
                Some(unit)
            }
            _ => None,
        };
        match value {
            Some(value) => Ok(value),
            None if self.is_unicode => Err(Error::InvalidIdentityEscape),
            None => Ok(unit),
        }
    }

    /// After the first digit.
    fn octal(&mut self, first: u32) -> u32 {
        let mut value = first;
        for _ in 0..if first < 4 { 2 } else { 1 } {
            let digit @ b'0'..=b'7' = self.peek() else {
                break;
            };
            value = value * 8 + u32::from(digit - b'0');
            self.at += 1;
        }
        value
    }

    /// After `\c`.
    fn control_escape(&mut self, context: Context) -> Result<u32, Error> {
        let letter = self.peek();
        if !letter.is_ascii_alphabetic() {
            if self.is_unicode {
                return Err(match self.is_at_end() {
                    true => Error::InvalidIdentityEscape,
                    false => Error::InvalidControlEscape,
                });
            }
            if context == Context::Atom || !matches!(letter, b'0'..=b'9' | b'_') {
                self.at -= 1;
                return Ok(u32::from(b'\\'));
            }
        }
        self.at += 1;
        Ok(u32::from(letter & 0x1f))
    }

    /// Moves only if there are that many digits.
    #[inline]
    fn hexadecimal(&mut self, count: usize) -> Option<u32> {
        let digits = self.units.get(self.at..self.at + count)?;
        let value = digits
            .iter()
            .try_fold(0, |value, unit| Some(value << 4 | unit.digit_value(16)?))?;
        self.at += count;
        Some(value)
    }

    /// RegExpUnicodeEscapeSequence[+UnicodeMode], after the backslash.
    pub(super) fn strict_unicode_escape(&mut self) -> Result<u32, Error> {
        if !self.eat(b'u') {
            return Err(Error::InvalidUnicodeEscape);
        }
        if self.eat(b'{') {
            return self.code_point().ok_or(Error::InvalidCodePointEscape);
        }
        let unit = self.hexadecimal(4).ok_or(Error::InvalidUnicodeEscape)?;
        if is_lead_surrogate(unit) && self.peek() == b'\\' && self.peek_at(1) == b'u' {
            let start = self.at;
            self.at += 2;
            match self.hexadecimal(4).filter(|&it| is_trail_surrogate(it)) {
                Some(trail) => return Ok(join_surrogates(unit, trail)),
                None => self.at = start,
            }
        }
        Ok(unit)
    }

    /// After `\u{`.
    fn code_point(&mut self) -> Option<u32> {
        let mut value = 0;
        loop {
            value = value << 4 | self.peek().digit_value(16)?;
            if value > MAX_CODE_POINT {
                return None;
            }
            self.at += 1;
            if self.eat(b'}') {
                return Some(value);
            }
        }
    }

    /// At the letter.
    fn property(&mut self) -> Result<ClassEscape, Error> {
        let negated = self.peek() == b'P';
        self.at += 1;
        let property = self.property_expression().ok_or(Error::InvalidProperty)?;
        if negated && property.has_strings() {
            return Err(Error::NegatedStrings);
        }
        let kind = ClassKind::Property(property);
        Ok(ClassEscape { kind, negated })
    }

    fn property_expression(&mut self) -> Option<Property> {
        let mut word_after = |before: u8| {
            self.eat(before).then(|| {
                let start = self.at;
                while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                    self.at += 1;
                }
                C::text(&self.units[start..self.at])
            })
        };
        let name = word_after(b'{')?;
        let value = word_after(b'=');
        if !self.eat(b'}') {
            return None;
        }
        match value {
            Some(value) => Property::with_value(name, value),
            None => Property::of_characters(name)
                .or_else(|| Property::of_strings(name).filter(|_| self.has_class_sets)),
        }
    }
}
