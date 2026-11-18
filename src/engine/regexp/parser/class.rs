//! Classes without the flag `v`.

use bun_engine_text::CodeUnit;

use super::escape::{ClassAtom, Context};
use super::quantifier::Atom;
use super::{Cursor, Parser, Sink};
use crate::Error;

impl<C: CodeUnit> Cursor<'_, C> {
    /// `unit` is before the cursor.
    #[inline(always)]
    fn class_atom(&mut self, unit: u32) -> Result<ClassAtom, Error> {
        match unit == u32::from(b'\\') {
            true => self.escape(Context::Class),
            false => Ok(ClassAtom::Character(self.complete_character(unit))),
        }
    }
}

impl<S: Sink> Parser<'_, S> {
    /// After the left bracket.
    #[inline(always)]
    pub(super) fn class<C: CodeUnit>(
        &mut self,
        cursor: &mut Cursor<C>,
    ) -> Result<Atom<S::Outer>, Error> {
        let negated = cursor.eat(b'^');
        let mut before_hyphen = None;
        loop {
            let unit = cursor.next_unit().ok_or(Error::UnterminatedClass)?;
            if unit == u32::from(b']') {
                return Ok(Atom::Class { negated });
            }
            let atom = cursor.class_atom(unit)?;
            match (before_hyphen.take(), atom) {
                (None, _) if cursor.peek() == b'-' && cursor.peek_at(1) != b']' => {
                    cursor.at += 1;
                    before_hyphen = Some(atom);
                }
                (None, _) => self.add_to_class(atom)?,
                (Some(ClassAtom::Character(first)), ClassAtom::Character(last)) => {
                    self.class_range(first, last)?;
                }
                _ if cursor.is_unicode => return Err(Error::InvalidRange),
                (Some(first), last) => {
                    self.add_to_class(first)?;
                    self.sink.class_character(u32::from(b'-'))?;
                    self.add_to_class(last)?;
                }
            }
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
