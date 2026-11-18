//! Finds where tokens end and checks them. What a literal stands for is left to whoever asks.

mod comment;
mod name;
mod number;
mod quoted;

use bun_highway::{ScannerStops, ScannerWindow, Stops};

use crate::chars::{self, Start};
use crate::error::LexicalError;
use crate::names::{Name, Names};
use crate::source::Source;
use crate::token::{Token, TokenKind};

pub(crate) use quoted::TEMPLATE_IS_TAIL;

#[repr(C)]
pub(crate) struct Lexer<'a> {
    pub(crate) token: Token,
    pub(crate) previous_end: u32,
    /// The index of the last name that is reserved.
    last_keyword: u32,
    window_start: usize,
    /// Those of `source`, up to the end of what is parsed.
    bytes: &'a [u8],
    pub(crate) source: Source<'a>,
    pub(crate) names: Names,
    first_offset: u32,
    is_module: bool,
    is_builtin: bool,
    /// Outlasts the token it is about, and is reported in place of what the parser has to say.
    pub(crate) error: Option<LexicalError>,
    pub(crate) source_url: Option<(u32, u32)>,
    pub(crate) source_mapping_url: Option<(u32, u32)>,
    pub(crate) scratch: Vec<u16>,
    window: ScannerWindow,
}

/// What is done with a name that has nothing but ASCII letters and digits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Naming {
    LookUp,
    /// An identifier without a value, whatever it spells.
    Skip,
}

impl<'a> Lexer<'a> {
    /// There is no token until [`Lexer::next`].
    pub(crate) fn new(
        source: Source<'a>,
        (start, end): (u32, u32),
        names: Names,
        scratch: Vec<u16>,
        is_module: bool,
        is_builtin: bool,
    ) -> Lexer<'a> {
        Lexer {
            token: Token {
                start,
                end: start,
                value: 0,
                kind: TokenKind::Semicolon,
                follows_newline: false,
                is_elaborate: false,
            },
            previous_end: start,
            last_keyword: Name::AWAIT.index() as u32,
            window_start: usize::MAX / 2,
            bytes: &source.bytes[..end as usize],
            source,
            names,
            first_offset: start,
            is_module,
            is_builtin,
            error: None,
            source_url: None,
            source_mapping_url: None,
            scratch,
            window: ScannerWindow::default(),
        }
    }

    #[inline(always)]
    pub(crate) fn set_strict(&mut self, is_strict: bool) {
        let last = if is_strict { Name::STATIC } else { Name::AWAIT };
        self.last_keyword = last.index() as u32;
    }

    #[inline(always)]
    fn is_strict(&self) -> bool {
        self.last_keyword != Name::AWAIT.index() as u32
    }

    /// Of what is parsed.
    #[inline(always)]
    pub(crate) fn end(&self) -> u32 {
        self.bytes.len() as u32
    }

    /// Zero past the end.
    #[inline(always)]
    pub(crate) fn byte(&self, offset: usize) -> u8 {
        self.bytes.get(offset).copied().unwrap_or(0)
    }

    #[inline(always)]
    pub(crate) fn bytes_of(&self, token: Token) -> &'a [u8] {
        &self.bytes[token.start as usize..token.end as usize]
    }

    #[inline(always)]
    pub(crate) fn next(&mut self) {
        self.advance(Naming::LookUp);
    }

    /// For where any name will do. With `NEEDS_VALUE` it may still be a keyword.
    #[inline(always)]
    pub(crate) fn next_key<const NEEDS_VALUE: bool>(&mut self) {
        self.advance(if NEEDS_VALUE {
            Naming::LookUp
        } else {
            Naming::Skip
        });
    }

    /// No name is a keyword.
    #[inline(always)]
    pub(crate) fn next_ignoring_keywords<const NEEDS_VALUE: bool>(&mut self) {
        self.next_key::<NEEDS_VALUE>();
        if self.token.kind.is_identifier_name() {
            self.token.kind = TokenKind::Identifier;
        }
    }

    /// What [`Lexer::next`] would have made of a token of [`Lexer::next_key`].
    pub(crate) fn looked_up(&mut self, mut token: Token) -> Token {
        if token.kind == TokenKind::Identifier && token.value == 0 {
            let name =
                (self.names).intern_source(self.source, token.start as usize, token.end as usize);
            token.value = name.index() as u32;
            token.kind = TokenKind::of_name(name, self.last_keyword);
        }
        token
    }

    /// Goes back to a token that has been seen before.
    #[inline(always)]
    pub(crate) fn restore(&mut self, token: Token, previous_end: u32) {
        self.token = token;
        self.previous_end = previous_end;
        self.error = None;
    }

    /// Reads the token that starts at `offset`, as if the one before had ended there.
    pub(crate) fn jump(&mut self, offset: u32) {
        self.token.end = offset;
        self.error = None;
        self.next();
    }

    /// Reads the token once more, by the rules that hold now.
    pub(crate) fn read_again(&mut self) {
        self.error = None;
        self.scan(self.token.start as usize, Naming::LookUp);
    }

    #[inline(never)]
    fn advance(&mut self, naming: Naming) {
        let offset = self.token.end;
        self.previous_end = offset;
        self.token.follows_newline = false;
        self.scan(offset as usize, naming);
    }

    #[inline(always)]
    fn set(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.token.kind = kind;
        self.token.start = start as u32;
        self.token.end = end as u32;
    }

    #[cold]
    #[inline(never)]
    fn fail(&mut self, kind: TokenKind, start: usize, end: usize, message: &'static str) {
        self.error = Some(LexicalError::Verbatim(message));
        self.set(kind, start, end);
    }

    /// Of a literal that is well formed up to `end`.
    fn kind_of_malformed(&self, end: usize) -> TokenKind {
        if end >= self.bytes.len() {
            TokenKind::Unterminated
        } else {
            TokenKind::Invalid
        }
    }

    #[inline(never)]
    fn move_window(&mut self, offset: usize) {
        self.window
            .fill(self.bytes.get(offset..).unwrap_or_default());
        self.window_start = offset;
    }

    /// The first stop from `offset` on.
    #[inline(always)]
    fn stop(
        &mut self,
        mut offset: usize,
        plane: impl for<'w> Fn(ScannerStops<'w>) -> Stops<'w>,
    ) -> usize {
        loop {
            let mut position = offset.wrapping_sub(self.window_start);
            if position >= ScannerWindow::REACH {
                self.move_window(offset);
                position = 0;
            }
            let run = plane(self.window.stops()).run(position) as usize;
            if run < 57 {
                return offset + run;
            }
            offset += 56;
        }
    }

    /// Where the ASCII letters, digits, `_` and `$` end.
    #[inline(always)]
    pub(crate) fn skip_name(&mut self, offset: usize) -> usize {
        self.stop(offset, |stops| stops.after_name)
    }

    /// Two characters if the second is `=`.
    #[inline(always)]
    fn set_operator(&mut self, plain: TokenKind, assigning: TokenKind, start: usize, end: usize) {
        if self.byte(end) == b'=' {
            self.set(assigning, start, end + 1);
        } else {
            self.set(plain, start, end);
        }
    }

    #[inline(always)]
    fn scan(&mut self, mut offset: usize, naming: Naming) {
        use TokenKind::*;
        loop {
            let Some(&byte) = self.bytes.get(offset) else {
                return self.set(End, self.bytes.len(), self.bytes.len());
            };
            let next = offset + 1;
            match chars::start(byte) {
                Start::Name => return self.name(offset, naming),
                Start::Punctuator => self.set(chars::punctuator(byte), offset, next),
                Start::Blank => {
                    offset = next;
                    continue;
                }
                Start::Newline => {
                    self.token.follows_newline = true;
                    offset = self.stop(next, |stops| stops.after_blanks);
                    continue;
                }
                Start::Digit => return self.number(offset),
                Start::Quote => return self.string(offset, byte),
                Start::Dot => match self.byte(next) {
                    b'0'..=b'9' => return self.elaborate_number(offset),
                    b'.' if self.byte(next + 1) == b'.' => self.set(Ellipsis, offset, next + 2),
                    _ => self.set(Dot, offset, next),
                },
                Start::Equals => match self.byte(next) {
                    b'=' => self.set_operator(Equal, StrictEqual, offset, next + 1),
                    b'>' => self.set(Arrow, offset, next + 1),
                    _ => self.set(Assign, offset, next),
                },
                Start::Exclamation => match self.byte(next) {
                    b'=' => self.set_operator(NotEqual, StrictNotEqual, offset, next + 1),
                    _ => self.set(Not, offset, next),
                },
                Start::Question => match self.byte(next) {
                    b'?' => self.set_operator(Coalesce, CoalesceAssign, offset, next + 1),
                    b'.' if !self.byte(next + 1).is_ascii_digit() => {
                        self.set(QuestionDot, offset, next + 1);
                    }
                    _ => self.set(Question, offset, next),
                },
                Start::Less => match self.byte(next) {
                    b'<' => self.set_operator(ShiftLeft, ShiftLeftAssign, offset, next + 1),
                    b'!' if !self.is_module && self.bytes[next..].starts_with(b"!--") => {
                        offset = self.skip_line(next);
                        continue;
                    }
                    _ => self.set_operator(Less, LessEqual, offset, next),
                },
                Start::Greater => match (self.byte(next), self.byte(next + 1)) {
                    (b'>', b'>') => self.set_operator(
                        ShiftRightUnsigned,
                        ShiftRightUnsignedAssign,
                        offset,
                        next + 2,
                    ),
                    (b'>', _) => self.set_operator(ShiftRight, ShiftRightAssign, offset, next + 1),
                    _ => self.set_operator(Greater, GreaterEqual, offset, next),
                },
                Start::Plus => match self.byte(next) {
                    b'+' => self.set(Increment, offset, next + 1),
                    _ => self.set_operator(Plus, AddAssign, offset, next),
                },
                Start::Minus => match self.byte(next) {
                    b'-' if self.byte(next + 1) == b'>' && self.may_close_html_comment() => {
                        offset = self.skip_line(next);
                        continue;
                    }
                    b'-' => self.set(Decrement, offset, next + 1),
                    _ => self.set_operator(Minus, SubtractAssign, offset, next),
                },
                Start::Star => match self.byte(next) {
                    b'*' => self.set_operator(Exponentiate, ExponentiateAssign, offset, next + 1),
                    _ => self.set_operator(Star, MultiplyAssign, offset, next),
                },
                Start::Slash => match self.byte(next) {
                    b'/' => {
                        offset = self.line_comment(next + 1);
                        continue;
                    }
                    b'*' => match self.block_comment(offset) {
                        Some(end) => {
                            offset = end;
                            continue;
                        }
                        None => return,
                    },
                    _ => self.set_operator(Slash, DivideAssign, offset, next),
                },
                Start::Percent => self.set_operator(Percent, RemainderAssign, offset, next),
                Start::Ampersand => match self.byte(next) {
                    b'&' => self.set_operator(And, AndAssign, offset, next + 1),
                    _ => self.set_operator(BitAnd, BitAndAssign, offset, next),
                },
                Start::Bar => match self.byte(next) {
                    b'|' => self.set_operator(Or, OrAssign, offset, next + 1),
                    _ => self.set_operator(BitOr, BitOrAssign, offset, next),
                },
                Start::Caret => self.set_operator(BitXor, BitXorAssign, offset, next),
                Start::Other => {
                    if offset == 0 && self.bytes.starts_with(b"#!") {
                        offset = self.skip_line(next);
                        continue;
                    }
                    return self.unusual_name(offset);
                }
            }
            return;
        }
    }

    #[inline(always)]
    fn name(&mut self, start: usize, naming: Naming) {
        let end = self.skip_name(start);
        if chars::continues_name(self.byte(end)) {
            return self.elaborate_name(start, start, end);
        }
        self.token.is_elaborate = false;
        if naming == Naming::Skip {
            self.token.value = 0;
            return self.set(TokenKind::Identifier, start, end);
        }
        match self.names.intern_short(self.source, start, end - start) {
            Some(name) => self.set_name(name, start, end),
            None => self.long_name(start, end),
        }
    }

    #[inline(always)]
    fn set_name(&mut self, name: Name, start: usize, end: usize) {
        self.token.value = name.index() as u32;
        self.set(TokenKind::of_name(name, self.last_keyword), start, end);
    }
}
