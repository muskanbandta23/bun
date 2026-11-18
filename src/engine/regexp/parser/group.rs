use bun_engine_text::CodeUnit;

use super::names::Name;
use super::quantifier::Repeated;
use super::{GroupKind, MAX_DEPTH, Parser, Sink, try_push};
use crate::tree::{Group, Groups, MAX_GROUPS};
use crate::{Error, Flags};

/// A disjunction that has not ended yet.
#[derive(Clone, Copy)]
pub(super) struct Level {
    first_group: Group,
    groups_before_body: Group,
    groups_before_alternative: Group,
    repeated: Repeated,
    is_backward: bool,
}

impl Level {
    pub(super) const PATTERN: Level = Level {
        first_group: 1,
        groups_before_body: 0,
        groups_before_alternative: 0,
        repeated: Repeated::Nothing,
        is_backward: false,
    };
}

/// How a group that starts before the cursor relates to it.
pub(super) enum Relation {
    /// They are in different alternatives.
    Exclusive,
    Enclosing,
    /// It has ended, in an alternative that goes on, as have the groups from `first_sibling` up to it.
    Sibling {
        first_sibling: Group,
        is_backward: bool,
    },
}

impl<'a, C: CodeUnit, S: Sink> Parser<'a, C, S> {
    #[inline]
    pub(super) fn next_alternative(&mut self) -> Result<(), Error> {
        self.level.groups_before_alternative = self.group_count;
        self.sink.next_alternative()
    }

    #[inline]
    pub(super) fn open_group(&mut self) -> Result<(), Error> {
        let first_group = self.group_count + 1;
        let kind = match self.eat(b'?') {
            true => self.group_specifier()?,
            false => GroupKind::Capture(self.add_group()?),
        };
        let (repeated, is_backward) = match kind {
            GroupKind::Look { behind: true, .. } => {
                self.open_lookbehinds += 1;
                (Repeated::Lookbehind, true)
            }
            GroupKind::Look { .. } if self.is_unicode => (Repeated::Nothing, false),
            GroupKind::Look { .. } => (Repeated::Atom, false),
            _ => (Repeated::Atom, self.level.is_backward),
        };
        let outer = self.sink.open(kind)?;
        try_push(&mut self.outer_levels, (self.level, outer))?;
        self.is_too_deep |= self.outer_levels.len() > MAX_DEPTH;
        self.level = Level {
            first_group,
            groups_before_body: self.group_count,
            groups_before_alternative: self.group_count,
            repeated,
            is_backward,
        };
        Ok(())
    }

    #[inline]
    pub(super) fn close_group(&mut self) -> Result<(), Error> {
        let (level, outer) = self.outer_levels.pop().ok_or(Error::UnmatchedParenthesis)?;
        let closed = core::mem::replace(&mut self.level, level);
        self.open_lookbehinds -= usize::from(matches!(closed.repeated, Repeated::Lookbehind));
        let captures = Groups {
            first: closed.first_group,
            end: self.group_count + 1,
        };
        let quantifier = self.quantifier(closed.repeated)?;
        self.sink.close(outer, captures, quantifier)
    }

    #[inline]
    fn add_group(&mut self) -> Result<Group, Error> {
        if self.group_count == MAX_GROUPS {
            return Err(Error::TooManyCaptures);
        }
        self.group_count += 1;
        Ok(self.group_count)
    }

    /// After `(?`.
    #[inline]
    fn group_specifier(&mut self) -> Result<GroupKind, Error> {
        let look = |behind, negated| Ok(GroupKind::Look { behind, negated });
        let first = self.peek();
        self.at += 1;
        match first {
            b':' => Ok(GroupKind::Plain {
                set: Flags::empty(),
                cleared: Flags::empty(),
            }),
            b'=' => look(false, false),
            b'!' => look(false, true),
            b'<' if self.eat(b'=') => look(true, false),
            b'<' if self.eat(b'!') => look(true, true),
            b'<' => match self.group_name()? {
                Some(name) => self.named_group(name),
                None => Err(Error::InvalidGroupName),
            },
            b'-' | b'i' | b'm' | b's' => {
                self.at -= 1;
                self.modifiers()
            }
            _ => Err(Error::InvalidGroup),
        }
    }

    fn named_group(&mut self, name: Name<'a>) -> Result<GroupKind, Error> {
        if self.has_escaped_k {
            return Err(Error::InvalidNamedReference);
        }
        let name = self.names.find_or_add(name)?;
        // If the last group with the name cannot take part along with this one, no earlier one can.
        if let Some(last) = *self.names.last_group(name)
            && !matches!(self.relation(last), Relation::Exclusive)
        {
            return Err(Error::DuplicateGroupName);
        }
        let group = self.add_group()?;
        *self.names.last_group(name) = Some(group);
        self.has_named_groups = true;
        self.sink.name_group(group, name, self.names.text(name))?;
        Ok(GroupKind::Capture(group))
    }

    /// The last error before the colon is the one to report.
    fn modifiers(&mut self) -> Result<GroupKind, Error> {
        let (mut set, mut cleared) = (Flags::empty(), None);
        let mut error = None;
        while let Some(unit) = self.next_unit().filter(|&unit| unit != u32::from(b':')) {
            let flag = Flags::from_letter(unit as u16).filter(|&it| Flags::MODIFIABLE.contains(it));
            if let Some(flag) = flag {
                let flags = cleared.as_mut().unwrap_or(&mut set);
                if flags.contains(flag) {
                    error = Some(Error::InvalidModifier);
                }
                flags.insert(flag);
            } else if unit != u32::from(b'-') {
                error = Some(Error::InvalidGroup);
            } else if cleared.is_some() {
                error = Some(Error::InvalidModifier);
            } else {
                cleared = Some(Flags::empty());
            }
        }
        let cleared = cleared.unwrap_or_default();
        match error {
            Some(error) => Err(error),
            None if set.intersects(cleared) || (set | cleared).is_empty() => {
                Err(Error::InvalidModifier)
            }
            None => Ok(GroupKind::Plain { set, cleared }),
        }
    }

    pub(super) fn relation(&self, group: Group) -> Relation {
        let (common, inner) = if self.level.groups_before_body < group {
            (&self.level, None)
        } else {
            let outer = &self.outer_levels;
            let around = outer.partition_point(|(level, _)| level.groups_before_body < group);
            let inner = outer.get(around).map_or(&self.level, |(level, _)| level);
            (&outer[around - 1].0, Some(inner))
        };
        if inner.is_some_and(|inner| inner.first_group == group) {
            Relation::Enclosing
        } else if group <= common.groups_before_alternative {
            Relation::Exclusive
        } else {
            Relation::Sibling {
                first_sibling: common.groups_before_alternative + 1,
                is_backward: common.is_backward,
            }
        }
    }
}
