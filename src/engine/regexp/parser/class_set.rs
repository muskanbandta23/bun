//! Classes with the flag `v`.

use bun_engine_text::CodeUnit;
use smallvec::SmallVec;

use super::escape::{
    CLASS_SET_RESERVED_IF_DOUBLED, CLASS_SET_SYNTAX_CHARACTERS, ClassAtom, Context, is_in,
};
use super::quantifier::Repeated;
use super::{MAX_DEPTH, Parser, SetOperator, Sink, try_push};
use crate::Error;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Contents {
    Empty,
    OneOperand,
    Union,
    AfterOperator(SetOperator),
    Operation(SetOperator),
}

#[derive(Clone, Copy)]
struct Class {
    negated: bool,
    contents: Contents,
    may_contain_strings: bool,
}

impl Class {
    fn new(negated: bool) -> Class {
        Class {
            negated,
            contents: Contents::Empty,
            may_contain_strings: false,
        }
    }

    fn check_room_for_operand(self) -> Result<(), Error> {
        match self.contents {
            Contents::Operation(_) => Err(Error::InvalidSetOperation),
            _ => Ok(()),
        }
    }

    fn add_operand(&mut self, may_contain_strings: bool) {
        match self.contents {
            Contents::AfterOperator(SetOperator::Subtraction) => {}
            Contents::AfterOperator(SetOperator::Intersection) => {
                self.may_contain_strings &= may_contain_strings;
            }
            _ => self.may_contain_strings |= may_contain_strings,
        }
        self.contents = match self.contents {
            Contents::Empty => Contents::OneOperand,
            Contents::AfterOperator(operator) => Contents::Operation(operator),
            _ => Contents::Union,
        };
    }

    fn add_operator(&mut self, operator: SetOperator) -> Result<(), Error> {
        if self.contents != Contents::OneOperand && self.contents != Contents::Operation(operator) {
            return Err(Error::InvalidSetOperation);
        }
        self.contents = Contents::AfterOperator(operator);
        Ok(())
    }

    fn check_negation(self) -> Result<(), Error> {
        match self.negated && self.may_contain_strings {
            true => Err(Error::NegatedStrings),
            false => Ok(()),
        }
    }
}

impl<C: CodeUnit, S: Sink> Parser<'_, C, S> {
    /// After the left bracket.
    pub(super) fn class_set(&mut self) -> Result<(), Error> {
        let mut class = Class::new(self.eat(b'^'));
        let mut outer = SmallVec::<[Class; 8]>::new();
        loop {
            let unit = self.next_unit().ok_or(Error::UnterminatedClass)?;
            match unit.min(0xff) as u8 {
                b']' => {
                    class.check_negation()?;
                    if matches!(class.contents, Contents::AfterOperator(_)) {
                        return Err(Error::InvalidSetCharacter);
                    }
                    let Some(outer) = outer.pop() else {
                        let quantifier = self.quantifier(Repeated::Atom)?;
                        return self.sink.close_class(class.negated, quantifier);
                    };
                    self.sink.close_nested_class(class.negated)?;
                    let nested = core::mem::replace(&mut class, outer);
                    class.add_operand(nested.may_contain_strings);
                    self.reject_range()?;
                }
                b'[' => {
                    class.check_room_for_operand()?;
                    self.sink.open_nested_class()?;
                    try_push(&mut outer, class)?;
                    self.is_too_deep |= outer.len() > MAX_DEPTH;
                    class = Class::new(self.eat(b'^'));
                }
                first @ (b'-' | b'&') if self.is_at_end() || self.peek() == first => {
                    let operator = self.set_operator(first)?;
                    class.add_operator(operator)?;
                    self.sink.set_operator(operator)?;
                }
                b'\\' if self.peek() == b'q' && self.peek_at(1) == b'{' => {
                    class.check_room_for_operand()?;
                    class.add_operand(self.class_strings()?);
                    self.reject_range()?;
                }
                b'\\' => {
                    class.check_room_for_operand()?;
                    let atom = self.escape(Context::ClassSet)?;
                    self.operand_or_range(&mut class, atom)?;
                }
                _ => {
                    let c = self.class_set_character(unit)?;
                    class.check_room_for_operand()?;
                    self.operand_or_range(&mut class, ClassAtom::Character(c))?;
                }
            }
        }
    }

    /// After the first of its two characters.
    fn set_operator(&mut self, character: u8) -> Result<SetOperator, Error> {
        if !self.eat(character) {
            return Err(Error::UnterminatedClass);
        }
        if self.is_at_end() || self.peek() == character {
            return Err(Error::InvalidSetCharacter);
        }
        Ok(match character {
            b'-' => SetOperator::Subtraction,
            _ => SetOperator::Intersection,
        })
    }

    fn is_at_single_hyphen(&self) -> bool {
        self.peek() == b'-' && self.peek_at(1) != b'-' && self.at + 1 < self.units.len()
    }

    /// After a nested class or class strings.
    fn reject_range(&self) -> Result<(), Error> {
        match self.is_at_single_hyphen() {
            true => Err(Error::InvalidSetOperation),
            false => Ok(()),
        }
    }

    /// One that is not escaped. `unit` is before the cursor.
    fn class_set_character(&mut self, unit: u32) -> Result<u32, Error> {
        if is_in(CLASS_SET_SYNTAX_CHARACTERS, unit) {
            return Err(Error::InvalidSetCharacter);
        }
        if is_in(CLASS_SET_RESERVED_IF_DOUBLED, unit) && u32::from(self.peek()) == unit {
            return Err(Error::InvalidSetOperation);
        }
        Ok(self.complete_pair(unit))
    }

    /// After `first`.
    fn operand_or_range(&mut self, class: &mut Class, first: ClassAtom) -> Result<(), Error> {
        class.add_operand(matches!(first, ClassAtom::Class(escape) if escape.has_strings()));
        if !self.is_at_single_hyphen() {
            return self.add_to_class(first);
        }
        if let Contents::Operation(_) = class.contents {
            return Err(match first {
                ClassAtom::Character(_) => Error::InvalidSetOperation,
                ClassAtom::Class(_) => Error::InvalidSetCharacter,
            });
        }
        self.at += 1;
        match (first, self.range_end(*class)?) {
            (ClassAtom::Character(first), ClassAtom::Character(last)) => {
                class.contents = Contents::Union;
                self.class_range(first, last)
            }
            _ => Err(Error::InvalidRange),
        }
    }

    /// After the hyphen.
    fn range_end(&mut self, mut class: Class) -> Result<ClassAtom, Error> {
        let unit = self.next_unit().ok_or(Error::UnterminatedClass)?;
        match unit.min(0xff) as u8 {
            b']' => class.check_negation()?,
            b'&' if self.is_at_end() || self.peek() == b'&' => {
                class.add_operator(self.set_operator(b'&')?)?;
            }
            b'\\' if self.peek() == b'q' && self.peek_at(1) == b'{' => {
                self.class_strings()?;
            }
            b'\\' => return self.escape(Context::ClassSet),
            _ => return self.class_set_character(unit).map(ClassAtom::Character),
        }
        Err(Error::InvalidSetCharacter)
    }

    /// At the `q`. Returns MayContainStrings.
    fn class_strings(&mut self) -> Result<bool, Error> {
        self.at += 2;
        let (mut may_contain_strings, mut length) = (false, 0usize);
        loop {
            let unit = self.next_unit().ok_or(Error::UnterminatedClassStrings)?;
            let c = match unit.min(0xff) as u8 {
                end @ (b'|' | b'}') => {
                    self.sink.close_class_string()?;
                    may_contain_strings |= length != 1;
                    if end == b'}' {
                        return Ok(may_contain_strings);
                    }
                    length = 0;
                    continue;
                }
                b'\\' => self.character_escape(Context::ClassSet)?,
                _ => self.class_set_character(unit)?,
            };
            self.sink.class_string_character(c)?;
            length += 1;
        }
    }
}
