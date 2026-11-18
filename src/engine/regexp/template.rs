//! The replacement templates of GetSubstitution of ECMA-262.

use core::ops::Range;

use bun_engine_text::{AllocError, CodeUnit, StringBuilder, Text};
use smallvec::SmallVec;

use crate::GroupNames;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Part {
    /// Of the template.
    Literal(Range<usize>),
    Match,
    Before,
    After,
    /// From 1 up to the number of groups.
    Group(usize),
    /// Where the name is in the template.
    Name(Range<usize>),
}

struct Parts<'a> {
    text: Text<'a>,
    at: usize,
    group_count: usize,
    /// Cleared once no `>` is left.
    may_have_name: bool,
    pending: Option<Part>,
}

impl Parts<'_> {
    /// What the `$` at `dollar` starts, and where that ends.
    fn reference(&mut self, dollar: usize) -> Option<(Part, usize)> {
        let unit = self.text.get(dollar + 1)?;
        let part = match u8::try_from(unit).ok()? {
            b'$' => Part::Literal(dollar..dollar + 1),
            b'&' => Part::Match,
            b'`' => Part::Before,
            b'\'' => Part::After,
            b'<' if self.may_have_name => {
                let end = self.text.find_unit(b'>'.into(), dollar + 2);
                self.may_have_name = end.is_some();
                return end.map(|end| (Part::Name(dollar + 2..end), end + 1));
            }
            digit @ b'0'..=b'9' => {
                let first = usize::from(digit - b'0');
                let second = self.text.get(dollar + 2).and_then(|it| it.digit_value(10));
                let both = second.map(|second| first * 10 + second as usize);
                let (group, end) = match both.filter(|&both| both <= self.group_count) {
                    Some(both) => (both, dollar + 3),
                    None => (first, dollar + 2),
                };
                let exists = (1..=self.group_count).contains(&group);
                return exists.then_some((Part::Group(group), end));
            }
            _ => return None,
        };
        Some((part, dollar + 2))
    }
}

impl Iterator for Parts<'_> {
    type Item = Part;

    fn next(&mut self) -> Option<Part> {
        if let Some(part) = self.pending.take() {
            return Some(part);
        }
        let start = self.at;
        let mut from = start;
        while let Some(dollar) = self.text.find_unit(b'$'.into(), from) {
            let Some((part, end)) = self.reference(dollar) else {
                from = dollar + 1;
                continue;
            };
            self.at = end;
            return Some(match part {
                Part::Literal(dollar) => Part::Literal(start..dollar.end),
                part if start == dollar => part,
                part => {
                    self.pending = Some(part);
                    Part::Literal(start..dollar)
                }
            });
        }
        self.at = self.text.len();
        (start < self.at).then_some(Part::Literal(start..self.at))
    }
}

enum Piece<'a> {
    Literal(Text<'a>),
    Match,
    Before,
    After,
    Group(u16),
    OneOfGroups(&'a [u16]),
}

/// A template that is taken apart once for all the matches of a pattern.
pub struct Template<'a> {
    pieces: SmallVec<[Piece<'a>; 8]>,
}

impl<'a> Template<'a> {
    /// `has_names`: whether the match has an object for `groups`.
    pub fn parts(
        text: Text<'a>,
        group_count: usize,
        has_names: bool,
    ) -> impl Iterator<Item = Part> {
        Parts {
            text,
            at: 0,
            group_count,
            may_have_name: has_names,
            pending: None,
        }
    }

    pub fn new(
        text: Text<'a>,
        group_count: u16,
        names: &'a GroupNames,
    ) -> Result<Template<'a>, AllocError> {
        let mut pieces = SmallVec::new();
        for part in Template::parts(text, group_count.into(), !names.is_empty()) {
            pieces.try_reserve(1).map_err(|_| AllocError)?;
            pieces.push(match part {
                Part::Literal(range) => Piece::Literal(text.slice(range)),
                Part::Match => Piece::Match,
                Part::Before => Piece::Before,
                Part::After => Piece::After,
                Part::Group(group) => Piece::Group(group as u16),
                Part::Name(range) => {
                    Piece::OneOfGroups(names.find(text.slice(range)).unwrap_or_default())
                }
            });
        }
        Ok(Template { pieces })
    }

    /// `matched` and `captures` are what a match of the pattern in `subject` has left.
    pub fn append(
        &self,
        result: &mut StringBuilder,
        subject: Text<'_>,
        matched: Range<usize>,
        captures: &[u32],
    ) {
        let capture = |group: u16| {
            let index = (usize::from(group) - 1) * 2;
            let (start, end) = (captures[index], captures[index + 1]);
            (start != u32::MAX).then(|| subject.slice(start as usize..end as usize))
        };
        for piece in &self.pieces {
            let text = match *piece {
                Piece::Literal(text) => Some(text),
                Piece::Match => Some(subject.slice(matched.clone())),
                Piece::Before => Some(subject.slice(0..matched.start)),
                Piece::After => Some(subject.slice_from(matched.end)),
                Piece::Group(group) => capture(group),
                Piece::OneOfGroups(groups) => groups.iter().find_map(|&group| capture(group)),
            };
            if let Some(text) = text {
                result.push_text(text);
            }
        }
    }
}
