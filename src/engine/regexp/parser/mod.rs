//! The grammar of patterns: ECMA-262, 22.2.1 and B.1.2.

mod class;
mod class_set;
mod escape;
mod group;
mod names;
mod quantifier;
mod reference;
mod scan;
mod sink;

use bun_engine_text::{CodeUnit, Text, with_text};
use smallvec::SmallVec;

use crate::tree::Group;
use crate::{Error, Flags};
use group::Level;
use names::Names;
use quantifier::Repeated;
use scan::GroupTotals;
pub(crate) use sink::{
    Assertion, ClassEscape, ClassKind, Discard, GroupKind, NameId, Quantifier, SetOperator, Sink,
};

/// What the syntax of a pattern tells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Summary {
    pub group_count: u16,
    pub has_named_groups: bool,
    pub(crate) is_too_deep: bool,
    pub(crate) has_large_counts: bool,
}

impl Summary {
    /// Whether to build the pattern at once, for the error that this can end in.
    pub fn is_near_limits(self) -> bool {
        self.is_too_deep || self.has_large_counts
    }
}

/// The early errors of a literal.
pub fn check_syntax(pattern: Text<'_>, flags: Text<'_>) -> Result<(), Error> {
    let flags = Flags::parse(flags).ok_or(Error::InvalidFlags)?;
    summarize(pattern, flags).map(drop)
}

pub fn summarize(pattern: Text<'_>, flags: Flags) -> Result<Summary, Error> {
    parse(pattern, flags, &mut Discard)
}

pub(crate) fn parse(
    pattern: Text<'_>,
    flags: Flags,
    sink: &mut impl Sink,
) -> Result<Summary, Error> {
    with_text!(pattern, |units| Parser::new(units, flags, sink).pattern())
}

const MAX_DEPTH: usize = 50_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Token {
    Literal,
    Bar,
    OpenGroup,
    CloseGroup,
    Start,
    End,
    Dot,
    OpenClass,
    CloseBracket,
    Backslash,
    Quantifier,
}

static TOKENS: [Token; 256] = {
    let mut tokens = [Token::Literal; 256];
    tokens[b'|' as usize] = Token::Bar;
    tokens[b'(' as usize] = Token::OpenGroup;
    tokens[b')' as usize] = Token::CloseGroup;
    tokens[b'^' as usize] = Token::Start;
    tokens[b'$' as usize] = Token::End;
    tokens[b'.' as usize] = Token::Dot;
    tokens[b'[' as usize] = Token::OpenClass;
    tokens[b']' as usize] = Token::CloseBracket;
    tokens[b'}' as usize] = Token::CloseBracket;
    tokens[b'\\' as usize] = Token::Backslash;
    tokens[b'*' as usize] = Token::Quantifier;
    tokens[b'+' as usize] = Token::Quantifier;
    tokens[b'?' as usize] = Token::Quantifier;
    tokens[b'{' as usize] = Token::Quantifier;
    tokens
};

#[inline(always)]
fn token<C: CodeUnit>(unit: C) -> Token {
    let known = TOKENS.get(usize::from(unit.to_u16()));
    known.copied().unwrap_or(Token::Literal)
}

struct Parser<'a, C, S: Sink> {
    units: &'a [C],
    at: usize,
    /// With `u` or `v`.
    is_unicode: bool,
    has_class_sets: bool,
    sink: &'a mut S,
    level: Level,
    outer_levels: SmallVec<[(Level, S::Outer); 16]>,
    open_lookbehinds: usize,
    is_too_deep: bool,
    group_count: Group,
    totals: Option<GroupTotals>,
    names: Names<'a>,
    has_named_groups: bool,
    /// An error if any group has a name.
    has_escaped_k: bool,
    highest_reference: u32,
    sum_of_counts: u64,
}

impl<'a, C: CodeUnit, S: Sink> Parser<'a, C, S> {
    #[inline]
    fn new(units: &'a [C], flags: Flags, sink: &'a mut S) -> Self {
        Parser {
            units,
            at: 0,
            is_unicode: flags.is_unicode_aware(),
            has_class_sets: flags.contains(Flags::UNICODE_SETS),
            sink,
            level: Level::PATTERN,
            outer_levels: SmallVec::new(),
            open_lookbehinds: 0,
            is_too_deep: false,
            group_count: 0,
            totals: None,
            names: Names::default(),
            has_named_groups: false,
            has_escaped_k: false,
            highest_reference: 0,
            sum_of_counts: 0,
        }
    }

    fn pattern(mut self) -> Result<Summary, Error> {
        if scan::is_too_large(self.units, self.has_class_sets) {
            return Err(Error::PatternTooLarge);
        }
        while let Some(&unit) = self.units.get(self.at) {
            self.at += 1;
            match token(unit) {
                Token::Literal => self.text()?,
                Token::Bar => self.next_alternative()?,
                Token::OpenGroup => self.open_group()?,
                Token::CloseGroup => self.close_group()?,
                Token::Start => self.sink.assertion(Assertion::Start)?,
                Token::End => self.sink.assertion(Assertion::End)?,
                Token::Dot => {
                    self.sink.dot()?;
                    self.quantify()?;
                }
                Token::OpenClass if self.has_class_sets => self.class_set()?,
                Token::OpenClass => self.class()?,
                Token::CloseBracket if self.is_unicode => return Err(Error::LoneBracket),
                Token::CloseBracket => self.text()?,
                Token::Backslash => self.atom_escape()?,
                Token::Quantifier => {
                    self.at -= 1;
                    self.quantifier(Repeated::Nothing)?;
                    self.at += 1;
                    self.text()?;
                }
            }
        }
        if !self.outer_levels.is_empty() {
            return Err(Error::UnterminatedGroup);
        }
        self.check_references()?;
        let counted_units = self.sum_of_counts.saturating_mul(2);
        Ok(Summary {
            group_count: self.group_count,
            has_named_groups: self.has_named_groups,
            is_too_deep: self.is_too_deep,
            has_large_counts: counted_units > u64::from(u32::MAX) - self.units.len() as u64,
        })
    }

    /// The unit before the cursor is the first.
    #[inline]
    fn text(&mut self) -> Result<(), Error> {
        let start = self.at - 1;
        let rest = self.units[self.at..].iter();
        self.at += rest
            .take_while(|&&unit| token(unit) == Token::Literal)
            .count();
        self.sink.text(&self.units[start..self.at])?;
        self.quantify()
    }

    /// An ASCII character as it is. The end and all other units are values above 0x7f.
    #[inline(always)]
    fn peek(&self) -> u8 {
        self.peek_at(0)
    }

    #[inline(always)]
    fn peek_at(&self, offset: usize) -> u8 {
        let unit = self.units.get(self.at + offset);
        unit.map_or(0xff, |unit| unit.to_u16().min(0xff) as u8)
    }

    #[inline(always)]
    fn is_at_end(&self) -> bool {
        self.at >= self.units.len()
    }

    #[inline(always)]
    fn eat(&mut self, ascii: u8) -> bool {
        let is_next = self.peek() == ascii;
        self.at += usize::from(is_next);
        is_next
    }

    #[inline(always)]
    fn next_unit(&mut self) -> Option<u32> {
        let unit = self.units.get(self.at)?;
        self.at += 1;
        Some(unit.to_u32())
    }

    /// Takes the second half if `unit`, which is before the cursor, starts a surrogate pair.
    #[inline(always)]
    fn complete_pair(&mut self, unit: u32) -> u32 {
        if !C::IS_LATIN1
            && is_lead_surrogate(unit)
            && let Some(trail) = self.units.get(self.at).map(|it| it.to_u32())
            && is_trail_surrogate(trail)
        {
            self.at += 1;
            return join_surrogates(unit, trail);
        }
        unit
    }

    /// The character that `unit`, which is before the cursor, starts.
    #[inline(always)]
    fn complete_character(&mut self, unit: u32) -> u32 {
        match self.is_unicode {
            true => self.complete_pair(unit),
            false => unit,
        }
    }

    /// Saturates.
    fn decimal(&mut self) -> u64 {
        let mut value = 0u64;
        while let digit @ b'0'..=b'9' = self.peek() {
            value = value
                .saturating_mul(10)
                .saturating_add(u64::from(digit - b'0'));
            self.at += 1;
        }
        value
    }
}

#[inline(always)]
fn is_lead_surrogate(unit: u32) -> bool {
    unit & !0x3ff == 0xd800
}

#[inline(always)]
fn is_trail_surrogate(unit: u32) -> bool {
    unit & !0x3ff == 0xdc00
}

#[inline(always)]
fn join_surrogates(lead: u32, trail: u32) -> u32 {
    0x10000 + ((lead - 0xd800) << 10) + (trail - 0xdc00)
}

#[inline]
pub(crate) fn try_push<A: smallvec::Array>(
    stack: &mut SmallVec<A>,
    item: A::Item,
) -> Result<(), Error> {
    stack.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
    stack.push(item);
    Ok(())
}
