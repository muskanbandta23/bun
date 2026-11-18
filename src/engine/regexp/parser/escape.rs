use bun_engine_text::CodeUnit;
use bun_engine_unicode::{MAX_CODE_POINT, Property};

use super::cursor::{Cursor, ascii, is_lead_surrogate, is_trail_surrogate, join_surrogates};
use super::{ClassEscape, ClassKind};
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

impl<C: CodeUnit> Cursor<'_, C> {
    /// After the backslash.
    #[inline(always)]
    pub(super) fn escape(&mut self, context: Context) -> Result<ClassAtom, Error> {
        let letter = self.peek();
        let kind = match letter {
            b'd' | b'D' => ClassKind::Digit,
            b's' | b'S' => ClassKind::Space,
            b'w' | b'W' => ClassKind::Word,
            b'p' | b'P' if self.is_unicode => {
                return self.apart(Self::property).map(ClassAtom::Class);
            }
            _ => return self.character_escape(context).map(ClassAtom::Character),
        };
        self.at += 1;
        let negated = letter < b'a';
        Ok(ClassAtom::Class(ClassEscape { kind, negated }))
    }

    /// After the backslash.
    #[inline(always)]
    pub(super) fn character_escape(&mut self, context: Context) -> Result<u32, Error> {
        let unit = self.next_unit().ok_or(Error::EscapeAtEnd)?;
        let value = match ascii(unit) {
            b'^' | b'$' | b'\\' | b'.' | b'*' | b'+' | b'?' | b'(' | b')' | b'[' | b']' | b'{'
            | b'}' | b'|' | b'/' => Some(unit),
            b't' => Some(0x09),
            b'n' => Some(0x0a),
            b'v' => Some(0x0b),
            b'f' => Some(0x0c),
            b'r' => Some(0x0d),
            b'x' => self.hexadecimal::<2>(),
            b'u' if self.is_unicode => return self.apart(Self::strict_unicode_escape),
            b'u' => self.hexadecimal::<4>(),
            b'b' if context != Context::Atom => Some(0x08),
            digit @ b'0'..=b'9' => return self.apart(|it| it.octal_escape(digit - b'0')),
            b'c' => return self.apart(|it| it.control_escape(context)),
            b'k' => {
                self.has_passed_escaped_k = true;
                None
            }
            b'-' if context != Context::Atom => Some(unit),
            b'&' | b'!' | b'#' | b'%' | b',' | b':' | b';' | b'<' | b'=' | b'>' | b'@' | b'`'
            | b'~'
                if context == Context::ClassSet =>
            {
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
    #[inline(never)]
    fn octal_escape(&mut self, first: u8) -> Result<u32, Error> {
        if first == 0 && !self.peek().is_ascii_digit() {
            return Ok(0);
        }
        if self.is_unicode {
            return Err(Error::InvalidOctalEscape);
        }
        if first > 7 {
            return Ok(u32::from(b'0' + first));
        }
        let mut value = u32::from(first);
        for _ in 0..if first < 4 { 2 } else { 1 } {
            let digit @ b'0'..=b'7' = self.peek() else {
                break;
            };
            value = value * 8 + u32::from(digit - b'0');
            self.at += 1;
        }
        Ok(value)
    }

    /// After `\c`.
    #[inline(never)]
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

    /// RegExpUnicodeEscapeSequence[+UnicodeMode], after `\u`.
    #[inline(never)]
    pub(super) fn strict_unicode_escape(&mut self) -> Result<u32, Error> {
        if self.eat(b'{') {
            return self.code_point().ok_or(Error::InvalidCodePointEscape);
        }
        let unit = self.hexadecimal::<4>().ok_or(Error::InvalidUnicodeEscape)?;
        if is_lead_surrogate(unit) && self.peek() == b'\\' && self.peek_at(1) == b'u' {
            let start = self.at;
            self.at += 2;
            match self.hexadecimal::<4>().filter(|&it| is_trail_surrogate(it)) {
                Some(trail) => return Ok(join_surrogates(unit, trail)),
                None => self.at = start,
            }
        }
        Ok(unit)
    }

    /// After `\u{`.
    #[inline(always)]
    fn code_point(&mut self) -> Option<u32> {
        let mut value = self.hexadecimal::<1>()?;
        while let Some(digit) = self.hexadecimal::<1>() {
            value = value << 4 | digit;
            if value > MAX_CODE_POINT {
                return None;
            }
        }
        self.eat(b'}').then_some(value)
    }

    /// At the letter.
    #[inline(never)]
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
                C::text(self.units_since(start))
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
