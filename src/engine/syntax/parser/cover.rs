//! What looks like an expression at first: parameters of arrow functions, and patterns that are assigned to.

use super::failure::Mark;
use super::pattern::Binder;
use super::{Class, Context, Expression, Failed, In, Parsed, Session};
use crate::ast::ExprKind;
use crate::cache::Traits;
use crate::chars::{self, Start};
use crate::names::Name;
use crate::options::{FunctionFlags, FunctionKind};
use crate::token::{Token, TokenKind};

/// Whether an arrow follows what is in parentheses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Guess {
    Yes,
    No,
    Unknown,
}

/// In a list of parameters or in a pattern.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    BeforeElement,
    AfterName,
    AfterColon,
}

fn arrow_flags(is_async: bool) -> FunctionFlags {
    if is_async {
        FunctionFlags::ASYNC
    } else {
        FunctionFlags::empty()
    }
}

impl Session<'_> {
    /// At the arrow.
    #[inline(never)]
    pub(super) fn arrow_after_name<const BUILD: bool>(
        &mut self,
        name: Token,
        r#in: In,
    ) -> Parsed<Expression> {
        self.lexer.restore(name, name.start());
        self.arrow_function::<BUILD>(name.start(), FunctionFlags::empty(), r#in)
    }

    #[inline(never)]
    pub(super) fn assignment_from_paren<const BUILD: bool>(
        &mut self,
        r#in: In,
    ) -> Parsed<Expression> {
        let mark = self.mark();
        let guess = self.guess_arrow(mark.token.end());
        self.expression_or_arrow::<BUILD>(&mark, guess, false, r#in)
    }

    /// One that is a keyword in some places, `arguments` or `eval`.
    #[inline(never)]
    pub(super) fn assignment_from_word<const BUILD: bool>(
        &mut self,
        r#in: In,
    ) -> Parsed<Expression> {
        if self.at(TokenKind::Yield) && self.is_yield_reserved() {
            return self.yield_expression::<BUILD>(r#in);
        }
        let mark = self.mark();
        if !self.at_contextual(Name::ASYNC) {
            return self.expression_or_arrow::<BUILD>(&mark, Guess::No, false, r#in);
        }
        self.next();
        let guess = match self.kind() {
            _ if self.follows_newline() => Guess::No,
            TokenKind::OpenParen => self.guess_arrow(self.token().end()),
            TokenKind::Identifier if self.peek().kind() == TokenKind::Arrow => Guess::Yes,
            _ => Guess::No,
        };
        if guess != Guess::Yes {
            self.rewind(&mark);
        }
        self.expression_or_arrow::<BUILD>(&mark, guess, true, r#in)
    }

    /// At the parameters if the guess is yes, else at `mark`.
    #[inline(always)]
    fn expression_or_arrow<const BUILD: bool>(
        &mut self,
        mark: &Mark,
        guess: Guess,
        is_async: bool,
        r#in: In,
    ) -> Parsed<Expression> {
        let start = mark.token.start();
        if guess == Guess::Yes
            && let Some(arrow) = self.presumed_arrow::<BUILD>(mark, is_async, r#in)
        {
            return arrow;
        }
        let assignments = self.assignments;
        let tentative = (guess != Guess::No).then(|| self.scopes.begin_tentative());
        let left = self.conditional::<BUILD>(r#in);
        match left {
            Ok(left) if !self.at(TokenKind::Arrow) => {
                if let Some(tentative) = tentative {
                    self.scopes.commit_tentative(tentative);
                }
                self.rest_of_assignment::<BUILD>(start, left, assignments, r#in)
            }
            _ => self.arrow_after_all::<BUILD>(mark, r#in),
        }
    }

    /// None if the parameters are no good. Which error to report is found out the long way, from `mark`.
    fn presumed_arrow<const BUILD: bool>(
        &mut self,
        mark: &Mark,
        is_async: bool,
        r#in: In,
    ) -> Option<Parsed<Expression>> {
        let flags = arrow_flags(is_async);
        match self.begin_function(FunctionKind::Arrow, flags, None, None) {
            Ok(begun) => Some(self.rest_of_arrow::<BUILD>(mark.token.start(), begun, r#in)),
            Err(Failed) if self.may_try_again() => {
                self.rewind(mark);
                None
            }
            Err(Failed) => Some(Err(Failed)),
        }
    }

    /// The expression from `mark` on has failed or runs into an arrow.
    #[cold]
    #[inline(never)]
    fn arrow_after_all<const BUILD: bool>(&mut self, mark: &Mark, r#in: In) -> Parsed<Expression> {
        let is_at_arrow = self.at(TokenKind::Arrow);
        if self.at(TokenKind::End)
            || (self.at(TokenKind::Await) && self.context.contains(Context::STATIC_BLOCK))
        {
            return Err(Failed);
        }
        if !self.may_try_again() {
            return Err(self.skip_attempt());
        }
        let attempt = self.abandon_attempt(mark);
        let mut is_async = false;
        if self.at_contextual(Name::ASYNC) {
            self.next();
            is_async = !self.follows_newline() && (self.at(TokenKind::OpenParen) || self.at_name());
            if !is_async {
                self.rewind(mark);
            }
        }
        match self.are_arrow_parameters() {
            Ok(true) => {
                return self.arrow_function::<BUILD>(
                    mark.token.start(),
                    arrow_flags(is_async),
                    r#in,
                );
            }
            Err(Failed) if is_at_arrow => return Err(Failed),
            _ => {}
        }
        self.resume_attempt(&attempt);
        self.is_hopeless = true;
        Err(self.unexpected())
    }

    fn are_arrow_parameters(&mut self) -> Parsed<bool> {
        if !self.at(TokenKind::OpenParen) {
            if !self.at_name() {
                return Ok(false);
            }
            self.refuse_await(self.token(), "parameter name")?;
            return Ok(self.peek().kind() == TokenKind::Arrow);
        }
        let mark = self.mark();
        let kept =
            Context::PARAMETERS | Context::NO_AWAIT_IN_PARAMETERS | Context::TOP_LEVEL_OF_MODULE;
        let context = (self.context).of_body(FunctionKind::Arrow, FunctionFlags::empty());
        let outer = self.enter_function(
            FunctionKind::Arrow,
            context | self.context & kept,
            mark.token.start(),
        );
        self.next();
        let result = self.parameter_list::<false>(FunctionKind::Arrow);
        self.abandon_function(&outer);
        result?;
        let are_parameters = self.eat(TokenKind::CloseParen) && self.at(TokenKind::Arrow);
        self.rewind(&mark);
        Ok(are_parameters)
    }

    /// From the characters alone. `offset` is past the opening parenthesis.
    #[inline(always)]
    fn guess_arrow(&mut self, offset: u32) -> Guess {
        let offset = offset as usize;
        match self.lexer.byte(offset) {
            b'(' | b'!' | b'"' | b'\'' | b'0'..=b'9' | b'-' | b'+' | b'`' => Guess::No,
            _ => self.look_for_arrow(offset),
        }
    }

    fn look_for_arrow(&mut self, start: usize) -> Guess {
        const REACH: usize = 256;
        const NO_DEFAULT: u32 = u32::MAX;
        let mut offset = start;
        // A bit for each bracket that is open, which is set for a brace.
        let (mut depth, mut braces) = (0, 0u32);
        let mut default_depth = NO_DEFAULT;
        let mut place = Place::BeforeElement;
        while offset - start < REACH && depth < u32::BITS {
            let byte = self.lexer.byte(offset);
            let is_plain = default_depth == NO_DEFAULT;
            let is_at_key = braces & 1 != 0 && place == Place::BeforeElement;
            offset += 1;
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => {}
                b'[' if is_plain && is_at_key => break,
                b'(' | b'[' | b'{' => {
                    if is_plain && (byte == b'(' || place == Place::AfterName) {
                        return Guess::No;
                    }
                    (depth, braces) = (depth + 1, braces << 1 | u32::from(byte == b'{'));
                    place = Place::BeforeElement;
                }
                b')' if depth == 0 => return self.look_at_arrow(offset),
                b']' | b'}' if depth == 0 => return Guess::No,
                b')' | b']' | b'}' | b',' => {
                    if default_depth == depth {
                        default_depth = NO_DEFAULT;
                    }
                    if byte == b',' {
                        place = Place::BeforeElement;
                    } else {
                        (depth, braces) = (depth - 1, braces >> 1);
                        place = Place::AfterName;
                    }
                }
                b'=' if is_plain => match self.lexer.byte(offset) {
                    b'=' | b'>' => return Guess::No,
                    _ => default_depth = depth,
                },
                b':' if is_plain && braces & 1 != 0 => place = Place::AfterColon,
                b'.' if place != Place::AfterName => {}
                b'"' | b'\'' if !is_plain || is_at_key => {
                    while !matches!(self.lexer.byte(offset), 0 | b'\\' | b'\n')
                        && offset - start < REACH
                    {
                        offset += 1;
                        if self.lexer.byte(offset - 1) == byte {
                            break;
                        }
                    }
                    place = Place::AfterName;
                }
                b'/' if is_plain && !matches!(self.lexer.byte(offset), b'/' | b'*') => {
                    return Guess::No;
                }
                b'<' if self.lexer.byte(offset) == b'!' => break,
                b'-' if self.lexer.byte(offset + 1) == b'>' => break,
                b'/' | b'`' | b'\\' | b'@' | 0x0b | 0x0c | 0x80.. => break,
                _ if chars::start(byte) == Start::Name || (byte.is_ascii_digit() && is_at_key) => {
                    if is_plain && place == Place::AfterName {
                        return Guess::No;
                    }
                    let end = self.lexer.skip_name(offset);
                    if end - offset == 4 && self.spells_await_or_yield(offset - 1) {
                        return Guess::Unknown;
                    }
                    (offset, place) = (end, Place::AfterName);
                }
                _ if is_plain || byte == 0 => return Guess::No,
                _ => {}
            }
        }
        self.guess_from_cache(start as u32 - 1)
    }

    fn spells_await_or_yield(&self, start: usize) -> bool {
        matches!(
            &self.lexer.source.bytes[start..start + 5],
            b"await" | b"yield"
        )
    }

    /// `offset` is past the closing parenthesis.
    fn look_at_arrow(&self, mut offset: usize) -> Guess {
        while self.lexer.byte(offset) == b' ' {
            offset += 1;
        }
        match (self.lexer.byte(offset), self.lexer.byte(offset + 1)) {
            (b'=', b'>') => Guess::Yes,
            (b'/', b'*' | b'/') | (b'\t' | 0x0b | 0x0c | 0xa0, _) => Guess::Unknown,
            _ => Guess::No,
        }
    }

    fn guess_from_cache(&self, parameters_start: u32) -> Guess {
        let is_known_arrow = self.functions.as_ref().is_some_and(|functions| {
            (functions.find(parameters_start))
                .is_some_and(|found| functions.summary(found).traits.contains(Traits::ARROW))
        });
        if is_known_arrow {
            Guess::Yes
        } else {
            Guess::Unknown
        }
    }

    #[inline(never)]
    pub(super) fn assignment_from_literal<const BUILD: bool>(
        &mut self,
        r#in: In,
    ) -> Parsed<Expression> {
        let (mark, assignments) = (self.mark(), self.assignments);
        match self.conditional::<BUILD>(r#in) {
            Ok(left) if left.class != Class::Literal || !self.at(TokenKind::Assign) => {
                self.rest_of_assignment::<BUILD>(mark.token.start(), left, assignments, r#in)
            }
            left => self.destructuring_assignment::<BUILD>(&mark, left.is_err(), r#in),
        }
    }

    #[inline(never)]
    fn destructuring_assignment<const BUILD: bool>(
        &mut self,
        mark: &Mark,
        is_no_literal: bool,
        r#in: In,
    ) -> Parsed<Expression> {
        if is_no_literal {
            if self.at(TokenKind::Await) && self.context.contains(Context::STATIC_BLOCK) {
                return Err(Failed);
            }
            if !self.may_try_again() {
                return Err(self.skip_attempt());
            }
        }
        let attempt = self.abandon_attempt(mark);
        let pattern = self.pattern::<BUILD>(Binder::Assignment);
        if is_no_literal && (pattern.is_err() || !self.at(TokenKind::Assign)) {
            self.resume_attempt(&attempt);
            self.is_hopeless = pattern.is_err();
            return Err(Failed);
        }
        let Ok(pattern) = pattern else {
            return Err(self.fail("Cannot parse assignment pattern"));
        };
        self.expect(
            TokenKind::Assign,
            "Expected '=' following assignment pattern",
        )?;
        let value = self.assignment_in::<BUILD>(r#in)?;
        let kind = ExprKind::Destructure {
            pattern,
            value: value.id(),
        };
        Ok(self.node::<BUILD>(value.class.of_wrapper(), mark.token.start(), kind))
    }
}
