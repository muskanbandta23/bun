use bun_engine_unicode::IgnoreCase;

use super::Builder;
use super::strings::{StringList, StringPool, Strings};
use crate::parser::{ClassEscape, SetOperator, Sink};
use crate::set::{Set, SetBuilder, SharedSet};
use crate::{Error, Flags};

/// The single characters of it.
pub(super) fn set_of_escape(escape: ClassEscape, flags: Flags) -> Result<Set, Error> {
    let folds_case = flags.contains(Flags::IGNORE_CASE) && flags.is_unicode_aware();
    let shared = match escape {
        ClassEscape::Digit => return Ok(Set::DIGIT),
        ClassEscape::NotDigit => return Ok(Set::NOT_DIGIT),
        ClassEscape::Word if folds_case => return Ok(Set::WORD_IGNORING_CASE),
        ClassEscape::Word => return Ok(Set::WORD),
        ClassEscape::NotWord if folds_case => return Ok(Set::WORD_IGNORING_CASE.complement()?),
        ClassEscape::NotWord => return Ok(Set::NOT_WORD),
        ClassEscape::Space => SharedSet::space(),
        ClassEscape::NotSpace => SharedSet::space().complement(),
        ClassEscape::Property(property) if folds_case => {
            SharedSet::of_property(property).closed_over_case()
        }
        ClassEscape::Property(property) => SharedSet::of_property(property),
        ClassEscape::NotProperty(property) => {
            let set = SharedSet::of_property(property);
            match (folds_case, flags.contains(Flags::UNICODE_SETS)) {
                (false, _) => set.complement(),
                (true, false) => set.complement().closed_over_case(),
                (true, true) => set.closed_over_case().complement(),
            }
        }
    };
    Ok(shared.to_set())
}

struct Members {
    characters: Set,
    strings: StringList,
}

#[derive(Default)]
struct Class {
    characters: SetBuilder,
    strings: StringList,
    left: Option<(Members, SetOperator)>,
}

/// The class that is open, and those around it.
#[derive(Default)]
pub(super) struct Classes {
    class: Class,
    outer: Vec<Class>,
    pool: StringPool,
    steps: usize,
}

/// More than a pattern takes that does not go through the same large sets again and again.
const MAX_STEPS: usize = 1 << 28;

impl Classes {
    #[inline]
    fn spend(&mut self, steps: usize) -> Result<(), Error> {
        self.steps += steps;
        match self.steps <= MAX_STEPS {
            true => Ok(()),
            false => Err(Error::PatternTooLarge),
        }
    }

    #[inline]
    pub(super) fn add_character(&mut self, c: u32) -> Result<(), Error> {
        Ok(self.class.characters.add(c)?)
    }

    #[inline]
    pub(super) fn add_range(&mut self, first: u32, last: u32) -> Result<(), Error> {
        Ok(self.class.characters.add_range(first, last)?)
    }

    pub(super) fn add_escape(&mut self, escape: ClassEscape, flags: Flags) -> Result<(), Error> {
        let set = set_of_escape(escape, flags)?;
        self.class.characters.add_set(&set)?;
        if let ClassEscape::Property(property) = escape {
            let ignores_case = flags.contains(Flags::IGNORE_CASE);
            let mut strings = property.strings();
            while let Some(string) = strings.next() {
                for &c in string {
                    self.pool.push(match ignores_case {
                        true => IgnoreCase::CodePoints.canonicalize(c),
                        false => c,
                    })?;
                }
                self.class.strings.push(self.pool.end_string())?;
                self.spend(string.len())?;
            }
        }
        Ok(())
    }

    pub(super) fn extend_string(&mut self, c: u32) -> Result<(), Error> {
        self.pool.push(c)
    }

    pub(super) fn close_string(&mut self) -> Result<(), Error> {
        match self.pool.take_single_character() {
            Some(c) => self.add_character(c),
            None => self.class.strings.push(self.pool.end_string()),
        }
    }

    pub(super) fn open_nested(&mut self) -> Result<(), Error> {
        self.outer.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
        self.outer.push(core::mem::take(&mut self.class));
        Ok(())
    }

    pub(super) fn close_nested(
        &mut self,
        negated: bool,
        ignore_case: Option<IgnoreCase>,
    ) -> Result<(), Error> {
        let members = self.members(negated, ignore_case)?;
        self.class = self.outer.pop().unwrap_or_default();
        self.spend(members.characters.owned_ranges() + members.strings.len())?;
        self.class.characters.add_set(&members.characters)?;
        self.class.strings.append(members.strings)
    }

    pub(super) fn set_operator(
        &mut self,
        operator: SetOperator,
        ignore_case: Option<IgnoreCase>,
    ) -> Result<(), Error> {
        self.class.left = Some((self.members(false, ignore_case)?, operator));
        Ok(())
    }

    #[inline]
    pub(super) fn close(
        &mut self,
        negated: bool,
        ignore_case: Option<IgnoreCase>,
    ) -> Result<(Set, Option<Strings>), Error> {
        let members = self.members(negated, ignore_case)?;
        if members.strings.len() == 0 {
            self.pool.clear();
            return Ok((members.characters, None));
        }
        let strings = Strings::new(core::mem::take(&mut self.pool), members.strings);
        Ok((members.characters, Some(strings)))
    }

    /// Leaves the class empty. One that is negated has no strings.
    #[inline]
    fn members(
        &mut self,
        negated: bool,
        ignore_case: Option<IgnoreCase>,
    ) -> Result<Members, Error> {
        self.spend(self.class.characters.cost())?;
        let mut members = Members {
            characters: self.class.characters.finish(ignore_case)?,
            strings: core::mem::take(&mut self.class.strings),
        };
        if let Some((left, operator)) = self.class.left.take() {
            members = self.combine(left, operator, members)?;
        }
        if negated {
            self.spend(members.characters.owned_ranges())?;
            members.characters = members.characters.complement()?;
        }
        Ok(members)
    }

    fn combine(
        &mut self,
        left: Members,
        operator: SetOperator,
        right: Members,
    ) -> Result<Members, Error> {
        self.spend(left.characters.above().len() + right.characters.above().len())?;
        let characters = match operator {
            SetOperator::Intersection => left.characters.intersection(&right.characters)?,
            SetOperator::Subtraction => left.characters.difference(&right.characters)?,
        };
        let mut strings = left.strings;
        let keeps_common = operator == SetOperator::Intersection;
        let steps = strings.filter(right.strings, keeps_common, &self.pool);
        self.spend(steps)?;
        Ok(Members {
            characters,
            strings,
        })
    }
}

impl Builder {
    /// As text where that says the same.
    #[inline]
    pub(super) fn add_class(&mut self, set: Set) -> Result<(), Error> {
        if let Some(c) = set.single() {
            return self.character(c);
        }
        if let Some(letter) = set.latin1().only_ascii_letter()
            && set.above().is_empty()
        {
            let equivalence = self.tree.case_equivalence();
            if equivalence == IgnoreCase::CodeUnits || !matches!(letter, b'k' | b's') {
                self.start_text(true)?;
                let canonical = equivalence.canonicalize(letter.into());
                return self.tree.extend_chars(1, [canonical].into_iter());
            }
        }
        self.add_set(set)
    }
}
