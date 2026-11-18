use bun_engine_text::CodeUnit;

use super::group::Relation;
use super::names::Name;
use super::scan::{GroupTotals, group_totals};
use super::{Parser, Sink};
use crate::Error;
use crate::tree::{Group, Groups, MAX_GROUPS};

impl<'a, C: CodeUnit, S: Sink> Parser<'a, C, S> {
    fn totals(&mut self) -> GroupTotals {
        *self.totals.get_or_insert_with(|| group_totals(self.units))
    }

    /// At the first digit. False, with the cursor where it was, if the digits are an escape of B.1.2.
    pub(super) fn back_reference(&mut self) -> Result<bool, Error> {
        let start = self.at;
        let number = u32::try_from(self.decimal()).unwrap_or(u32::MAX);
        let is_known = number <= u32::from(self.group_count);
        if !is_known && !self.is_unicode && number > self.totals().count {
            self.at = start;
            return Ok(false);
        }
        self.highest_reference = self.highest_reference.max(number);
        let group = Group::try_from(number)
            .ok()
            .filter(|&group| match is_known {
                true => matches!(
                    self.relation(group),
                    Relation::Sibling {
                        is_backward: false,
                        ..
                    }
                ),
                false => self.open_lookbehinds > 0,
            });
        self.sink.back_reference(group)?;
        Ok(true)
    }

    /// At the `k`. False, with the cursor where it was, if it stands for itself.
    pub(super) fn named_reference(&mut self) -> Result<bool, Error> {
        let start = self.at;
        self.at += 1;
        if self.eat(b'<')
            && (self.is_unicode || self.has_named_groups || self.totals().has_names)
            && let Some(name) = self.group_name()?
        {
            self.refer_to(name)?;
            return Ok(true);
        }
        self.at = start;
        Ok(false)
    }

    fn refer_to(&mut self, name: Name<'a>) -> Result<(), Error> {
        let name = self.names.find_or_add(name)?;
        let last = *self.names.last_group(name);
        match last.map(|last| (last, self.relation(last))) {
            Some((
                last,
                Relation::Sibling {
                    first_sibling: first,
                    is_backward: false,
                },
            )) => {
                let end = last + 1;
                self.sink.named_reference(name, Groups { first, end })
            }
            None | Some((_, Relation::Exclusive)) if self.open_lookbehinds > 0 => {
                let (first, end) = (1, MAX_GROUPS + 1);
                self.sink.named_reference(name, Groups { first, end })
            }
            _ => self.sink.back_reference(None),
        }
    }

    /// What only the end of the pattern decides.
    #[inline]
    pub(super) fn check_references(&self) -> Result<(), Error> {
        if self.highest_reference > u32::from(self.group_count) {
            return Err(Error::InvalidBackReference);
        }
        if (self.has_escaped_k && self.has_named_groups) || !self.names.all_have_groups() {
            return Err(Error::InvalidNamedReference);
        }
        Ok(())
    }
}
