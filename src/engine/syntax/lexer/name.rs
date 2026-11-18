//! Names that take more than a look at the first character that is no letter.

use bun_engine_text::Text;
use bun_engine_unicode::{MAX_CODE_POINT, is_identifier_part, is_identifier_start};

use super::Lexer;
use crate::chars;
use crate::error::LexicalError;
use crate::names::NameKind;
use crate::token::TokenKind;

pub(super) enum Malformed {
    /// More text could make it whole.
    Incomplete,
    Invalid,
}

fn is_surrogate(code_point: u32) -> bool {
    code_point & !0x7ff == 0xd800
}

impl Lexer<'_> {
    #[inline(never)]
    pub(super) fn long_name(&mut self, start: usize, end: usize) {
        let name = (self.names).intern_narrow(self.source, start, end, NameKind::Ordinary);
        self.set_name(name, start, end);
    }

    /// A lone surrogate is itself. Zero past the end.
    pub(super) fn code_point(&self, offset: usize) -> u32 {
        if offset >= self.bytes.len() {
            return 0;
        }
        (self.source.text.slice(0..self.bytes.len())).code_point_at(offset)
    }

    pub(super) fn starts_name(&self, offset: usize) -> bool {
        is_identifier_start(self.code_point(offset))
    }

    /// At a character that starts a name or no token at all.
    #[inline(never)]
    pub(super) fn unusual_name(&mut self, start: usize) {
        match self.byte(start) {
            b'#' if self.byte(start + 1) == b'\\' || self.starts_name(start + 1) => {
                let end = self.skip_name(start + 1);
                self.elaborate_name(start, start + 1, end);
            }
            b'@' if self.is_builtin => self.builtin_name(start),
            b'\\' => self.elaborate_name(start, start, start),
            _ if self.starts_name(start) => self.elaborate_name(start, start, start),
            _ => {
                let unit = self.source.text.at(start);
                self.error = Some(LexicalError::InvalidCharacter(unit));
                self.set(TokenKind::Invalid, start, start);
            }
        }
    }

    fn builtin_name(&mut self, start: usize) {
        let (kind, text_start) = match self.byte(start + 1) {
            b'@' => (NameKind::WellKnownSymbol, start + 2),
            _ => (NameKind::Private, start + 1),
        };
        let end = self.skip_name(text_start);
        let name = self.names.intern_narrow(self.source, text_start, end, kind);
        self.token.value = name.index() as u32;
        self.token.is_elaborate = false;
        self.set(TokenKind::Identifier, start, end);
    }

    /// `first` is past the `#` of a private name. All is well up to `offset`.
    #[inline(never)]
    pub(super) fn elaborate_name(&mut self, start: usize, first: usize, mut offset: usize) {
        /// From this far off the end, a keyword that runs into a character above 0xff is taken for an identifier.
        const LONGEST_KEYWORD: usize = 10;
        let mut has_escape = false;
        let mut may_be_keyword = true;
        while chars::continues_name(self.byte(offset)) {
            if self.byte(offset) == b'\\' {
                has_escape = true;
                match self.escape_in_name(offset + 1, offset == first) {
                    Ok(end) => offset = end,
                    Err((kind, end)) => return self.set(kind, start, end),
                }
            } else {
                let code_point = self.code_point(offset);
                if is_surrogate(code_point) {
                    return self.set(TokenKind::InvalidEncoding, start, offset);
                }
                if !is_identifier_part(code_point) {
                    if code_point > 0xffff {
                        return self.set(TokenKind::InvalidCodePoint, start, offset);
                    }
                    may_be_keyword = has_escape
                        || code_point <= 0xff
                        || self.bytes.len() - start <= LONGEST_KEYWORD;
                    break;
                }
                offset += if code_point > 0xffff { 2 } else { 1 };
            }
            offset = self.skip_name(offset);
        }
        let name = if has_escape {
            self.unescape_name(start, offset);
            (self.names).intern_text(self.source, Text::Utf16(&self.scratch))
        } else {
            self.names.intern_source(self.source, start, offset)
        };
        self.token.is_elaborate = has_escape;
        self.set_name(name, start, offset);
        if first != start {
            self.token.kind = TokenKind::PrivateName;
        } else if !may_be_keyword {
            self.token.kind = TokenKind::Identifier;
        } else if has_escape && self.token.kind != TokenKind::Identifier {
            self.token.kind = TokenKind::EscapedKeyword;
        }
    }

    /// `offset` is past the backslash.
    fn escape_in_name(&self, offset: usize, is_first: bool) -> Result<usize, (TokenKind, usize)> {
        if self.byte(offset) != b'u' {
            return Err(if offset >= self.bytes.len() {
                (TokenKind::IncompleteEscape, offset)
            } else {
                (TokenKind::InvalidEscape, offset)
            });
        }
        match self.unicode_escape(offset + 1) {
            (Ok(code_point), end) if is_first && is_identifier_start(code_point) => Ok(end),
            (Ok(code_point), end) if !is_first && is_identifier_part(code_point) => Ok(end),
            (Err(Malformed::Incomplete), end) => Err((TokenKind::IncompleteEscape, end)),
            (_, end) => Err((TokenKind::InvalidUnicodeEscape, end)),
        }
    }

    pub(super) fn skip_hex_digits(&self, mut offset: usize) -> usize {
        while self.byte(offset).is_ascii_hexdigit() {
            offset += 1;
        }
        offset
    }

    /// `offset` is past the `u`. What is malformed ends where NotEscapeSequence of ECMA-262 ends.
    pub(super) fn unicode_escape(&self, offset: usize) -> (Result<u32, Malformed>, usize) {
        let malformed = |is_incomplete| {
            Err(if is_incomplete {
                Malformed::Incomplete
            } else {
                Malformed::Invalid
            })
        };
        let value = |start: usize, end: usize| {
            self.bytes[start..end].iter().fold(0u32, |value, &digit| {
                let digit = char::from(digit).to_digit(16).unwrap_or(0);
                (value << 4 | digit).min(MAX_CODE_POINT + 1)
            })
        };
        if self.byte(offset) != b'{' {
            let is_whole = (offset..offset + 4).all(|at| self.byte(at).is_ascii_hexdigit());
            return if is_whole {
                (Ok(value(offset, offset + 4)), offset + 4)
            } else {
                let is_incomplete = offset + 4 >= self.bytes.len();
                (malformed(is_incomplete), self.skip_hex_digits(offset))
            };
        }
        let end = self.skip_hex_digits(offset + 1);
        let code_point = value(offset + 1, end);
        if code_point > MAX_CODE_POINT {
            (malformed(end >= self.bytes.len()), end)
        } else if end == offset + 1 || self.byte(end) != b'}' {
            (malformed(self.byte(end) == 0), end)
        } else {
            (Ok(code_point), end + 1)
        }
    }

    fn unescape_name(&mut self, mut offset: usize, end: usize) {
        self.scratch.clear();
        while offset < end {
            if self.bytes[offset] == b'\\' {
                let (code_point, next) = self.unicode_escape(offset + 2);
                self.push_code_point(code_point.unwrap_or(0));
                offset = next;
            } else {
                self.scratch.push(self.source.text.at(offset));
                offset += 1;
            }
        }
    }

    pub(super) fn push_code_point(&mut self, code_point: u32) {
        match u16::try_from(code_point) {
            Ok(unit) => self.scratch.push(unit),
            Err(_) => self.scratch.extend([
                (0xd7c0 + (code_point >> 10)) as u16,
                0xdc00 | (code_point & 0x3ff) as u16,
            ]),
        }
    }
}
