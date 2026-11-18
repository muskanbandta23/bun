//! Classes without the flag `v`.

use bun_engine_text::CodeUnit;

use super::escape::{ClassAtom, Context};
use super::quantifier::Repeated;
use super::{Parser, Sink};
use crate::Error;

impl<C: CodeUnit, S: Sink> Parser<'_, C, S> {
    /// After the left bracket.
    #[inline]
    pub(super) fn class(&mut self) -> Result<(), Error> {
        let negated = self.eat(b'^');
        loop {
            let unit = self.next_unit().ok_or(Error::UnterminatedClass)?;
            if unit == u32::from(b']') {
                break;
            }
            let first = self.class_atom(unit)?;
            if self.peek() != b'-' || self.peek_at(1) == b']' {
                self.add_to_class(first)?;
                continue;
            }
            self.at += 1;
            let unit = self.next_unit().ok_or(Error::UnterminatedClass)?;
            match (first, self.class_atom(unit)?) {
                (ClassAtom::Character(first), ClassAtom::Character(last)) => {
                    self.class_range(first, last)?;
                }
                _ if self.is_unicode => return Err(Error::InvalidRange),
                (first, last) => {
                    self.add_to_class(first)?;
                    self.sink.class_character(u32::from(b'-'))?;
                    self.add_to_class(last)?;
                }
            }
        }
        let quantifier = self.quantifier(Repeated::Atom)?;
        self.sink.close_class(negated, quantifier)
    }

    /// `unit` is before the cursor.
    #[inline(always)]
    fn class_atom(&mut self, unit: u32) -> Result<ClassAtom, Error> {
        match unit == u32::from(b'\\') {
            true => self.escape(Context::Class),
            false => Ok(ClassAtom::Character(self.complete_character(unit))),
        }
    }

    #[inline(always)]
    pub(super) fn add_to_class(&mut self, atom: ClassAtom) -> Result<(), Error> {
        match atom {
            ClassAtom::Character(c) => self.sink.class_character(c),
            ClassAtom::Class(escape) => self.sink.class_member(escape),
        }
    }

    #[inline(always)]
    pub(super) fn class_range(&mut self, first: u32, last: u32) -> Result<(), Error> {
        match first <= last {
            true => self.sink.class_range(first, last),
            false => Err(Error::RangeOutOfOrder),
        }
    }
}
