//! Finds where tokens end and checks them. What a literal stands for is left to whoever asks.

mod comment;
mod name;
mod number;
mod operator;
mod quoted;

use bun_highway::{ScannerStops, ScannerWindow, Stops};

use crate::chars;
use crate::error::LexicalError;
use crate::names::{Key, LONGEST_SHORT, Name, Names};
use crate::source::Source;
use crate::token::{Detail, Token, TokenKind};

use operator::SCANNERS;
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

/// The longest that `Stops::run` is sure of.
const EXACT_RUN: u32 = 56;

/// What is wrong with a literal.
#[derive(Clone, Copy)]
struct Flaw {
    kind: TokenKind,
    end: usize,
    message: &'static str,
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
        let mut lexer = Lexer {
            token: Token::new(
                TokenKind::Semicolon,
                start as usize,
                start as usize,
                Detail::NONE,
            ),
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
        };
        if start == 0 && lexer.bytes.starts_with(b"#!") {
            let end = lexer.skip_line(2);
            lexer.set(TokenKind::Semicolon, 0, end, Detail::NONE);
        }
        lexer
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
        &self.bytes[token.start() as usize..token.end() as usize]
    }

    #[inline(never)]
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
        if self.token.kind().is_identifier_name() {
            self.token = self.token.with_kind(TokenKind::Identifier);
        }
    }

    /// What [`Lexer::next`] would have made of a token of [`Lexer::next_key`].
    pub(crate) fn looked_up(&mut self, token: Token) -> Token {
        if token.kind() != TokenKind::Identifier || token.value() != 0 {
            return token;
        }
        let (start, end) = (token.start() as usize, token.end() as usize);
        let name = self.names.intern_source(self.source, start, end);
        self.word(name, start, end, token.lead())
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
        let offset = offset as usize;
        self.set(TokenKind::Semicolon, offset, offset, Detail::NONE);
        self.error = None;
        self.next();
    }

    /// Reads the token once more, by the rules that hold now.
    pub(crate) fn read_again(&mut self) {
        self.error = None;
        let (start, lead) = (self.token.start() as usize, self.token.lead());
        self.scan(start, lead, Naming::LookUp);
    }

    #[inline(always)]
    fn set(&mut self, kind: TokenKind, start: usize, end: usize, detail: Detail) {
        self.token = Token::new(kind, start, end, detail);
    }

    #[cold]
    #[inline(never)]
    fn fail(&mut self, start: usize, lead: Detail, flaw: Flaw) {
        self.error = Some(LexicalError::Verbatim(flaw.message));
        self.set(flaw.kind, start, flaw.end, lead);
    }

    /// Of a literal that is well formed up to `end`.
    fn flaw(&self, end: usize, message: &'static str) -> Flaw {
        let kind = if end >= self.bytes.len() {
            TokenKind::Unterminated
        } else {
            TokenKind::Invalid
        };
        Flaw { kind, end, message }
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
            if run <= EXACT_RUN as usize {
                return offset + run;
            }
            offset += EXACT_RUN as usize;
        }
    }

    /// Where the ASCII letters, digits, `_` and `$` end.
    #[inline(always)]
    pub(crate) fn skip_name(&mut self, offset: usize) -> usize {
        self.stop(offset, |stops| stops.after_name)
    }

    #[inline(always)]
    fn advance(&mut self, naming: Naming) {
        self.previous_end = self.token.end();
        self.scan(self.token.end() as usize, Detail::NONE, naming);
    }

    #[inline(always)]
    fn scan(&mut self, offset: usize, lead: Detail, naming: Naming) {
        match self.bytes.get(offset) {
            Some(&byte) => SCANNERS[usize::from(byte)](self, offset, lead, naming),
            None => self.set(TokenKind::End, self.bytes.len(), self.bytes.len(), lead),
        }
    }

    #[inline(always)]
    fn name(&mut self, start: usize, lead: Detail, naming: Naming) {
        let position = start.wrapping_sub(self.window_start);
        if position >= ScannerWindow::REACH {
            return self.any_name(start, lead, naming);
        }
        let length = self.window.stops().after_name.run(position) as usize;
        let chunk = match self.bytes[start..].first_chunk() {
            Some(chunk) if length <= LONGEST_SHORT && !chars::continues_name(chunk[length]) => {
                chunk
            }
            _ => return self.any_name(start, lead, naming),
        };
        if naming == Naming::Skip {
            return self.set(TokenKind::Identifier, start, start + length, lead);
        }
        let key = Key::short(chunk, length);
        match self.names.find_at_once(key) {
            Some(name) => {
                let kind = TokenKind::of_name(name, self.last_keyword);
                self.set(kind, start, start + length, lead | Detail::value(name));
            }
            None => self.short_name(key, start, start + length, lead),
        }
    }

    #[inline(always)]
    fn word(&self, name: Name, start: usize, end: usize, detail: Detail) -> Token {
        let name = name.index() as u32;
        let kind = TokenKind::of_name(name, self.last_keyword);
        Token::new(kind, start, end, detail | Detail::value(name))
    }
}
