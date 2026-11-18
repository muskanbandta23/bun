//! Strings, templates and regular expressions.

use bun_engine_text::Text;

use super::Lexer;
use crate::chars;
use crate::error::LexicalError;
use crate::names::Name;
use crate::source::WIDE_UNIT;
use crate::token::{Token, TokenKind};

/// In `Token::value`.
pub(crate) const TEMPLATE_IS_TAIL: u32 = 1;
const TEMPLATE_IS_MALFORMED: u32 = 2;

struct BadEscape {
    end: usize,
    message: &'static str,
}

impl Lexer<'_> {
    #[inline(never)]
    pub(super) fn string(&mut self, start: usize, quote: u8) {
        let end = self.stop(start + 1, |stops| stops.in_string);
        if self.byte(end) != quote {
            return self.rest_of_string(start, end, quote);
        }
        self.token.is_elaborate = false;
        self.set(TokenKind::String, start, end + 1);
    }

    #[inline(never)]
    fn rest_of_string(&mut self, start: usize, mut offset: usize, quote: u8) {
        self.token.is_elaborate = false;
        loop {
            match self.bytes.get(offset) {
                Some(&byte) if byte == quote => {
                    return self.set(TokenKind::String, start, offset + 1);
                }
                Some(b'\\') => {
                    self.token.is_elaborate = true;
                    match self.escape(offset + 1, self.is_strict()) {
                        Ok(end) => offset = end,
                        Err(bad) => return self.malformed_string(start, bad.end, bad.message),
                    }
                }
                Some(b'\n') if self.source.text.at(offset) > 0xff => offset += 1,
                Some(b'\n' | b'\r') | None => {
                    return self.malformed_string(start, offset, "Unexpected EOF");
                }
                Some(_) => offset = self.skip_stop(offset),
            }
            offset = self.stop(offset, |stops| stops.in_string);
        }
    }

    fn malformed_string(&mut self, start: usize, end: usize, message: &'static str) {
        let kind = if end >= self.bytes.len() {
            TokenKind::Unterminated
        } else {
            TokenKind::Invalid
        };
        self.fail(kind, start, end, message);
    }

    /// `offset` is past the backslash.
    #[inline]
    fn escape(&self, offset: usize, is_strict: bool) -> Result<usize, BadEscape> {
        let next = offset + 1;
        let bad = |end, message| Err(BadEscape { end, message });
        match self.bytes.get(offset) {
            Some(b'x') => {
                let end = self.skip_hex_digits(next);
                if end < next + 2 {
                    return bad(end, "\\x can only be followed by a hex character sequence");
                }
                Ok(next + 2)
            }
            Some(b'u') => match self.unicode_escape(next) {
                (Ok(_), end) => Ok(end),
                (Err(_), end) => bad(
                    end,
                    "\\u can only be followed by a Unicode character sequence",
                ),
            },
            Some(&digit @ b'0'..=b'9') if is_strict => {
                if digit != b'0' || self.byte(next).is_ascii_digit() {
                    let end = next + usize::from(digit == b'0');
                    return bad(end, "The only valid numeric escape in strict mode is '\\0'");
                }
                Ok(next)
            }
            Some(b'\r') if self.byte(next) == b'\n' => Ok(next + 1),
            Some(_) => Ok(next),
            None => bad(offset, "Unterminated string constant"),
        }
    }

    /// Makes of the current token, a backquote or a closing brace, the part of a template that it starts.
    pub(crate) fn scan_template(&mut self, is_tagged: bool) {
        let start = self.token.start as usize;
        let mut offset = self.token.end as usize;
        self.token.value = 0;
        self.token.is_elaborate = false;
        loop {
            offset = self.stop(offset, |stops| stops.in_string);
            match self.bytes.get(offset) {
                Some(b'`') => {
                    self.token.value |= TEMPLATE_IS_TAIL;
                    return self.set(TokenKind::Template, start, offset + 1);
                }
                Some(b'$') if self.byte(offset + 1) == b'{' => {
                    return self.set(TokenKind::Template, start, offset + 2);
                }
                Some(b'\\') => {
                    self.token.is_elaborate = true;
                    match self.escape(offset + 1, true) {
                        Ok(end) => offset = end,
                        Err(bad) if bad.end >= self.bytes.len() => {
                            let kind = TokenKind::UnterminatedMultiline;
                            return self.fail(kind, start, bad.end, bad.message);
                        }
                        Err(bad) if is_tagged => {
                            self.token.value |= TEMPLATE_IS_MALFORMED;
                            offset = bad.end;
                        }
                        Err(bad) => {
                            return self.fail(TokenKind::Invalid, start, bad.end, bad.message);
                        }
                    }
                }
                Some(b'\r') => {
                    self.token.is_elaborate = true;
                    offset += 1;
                }
                Some(_) => offset = self.skip_stop(offset),
                None => {
                    let kind = TokenKind::UnterminatedMultiline;
                    return self.fail(kind, start, offset, "Unexpected EOF");
                }
            }
        }
    }

    /// Makes of the current token, `/` or `/=`, the literal that it starts. Its value is where the flags start.
    pub(crate) fn scan_regexp(&mut self) {
        let start = self.token.start as usize;
        let mut offset = self.token.end as usize;
        let (mut is_escaped, mut is_in_class) = (false, false);
        loop {
            let byte = match self.bytes.get(offset) {
                Some(&byte) if !chars::is_newline(byte) => byte,
                _ => {
                    let error = LexicalError::UnterminatedRegExp(start as u32, offset as u32);
                    self.error = Some(error);
                    return self.set(TokenKind::Unterminated, start, offset);
                }
            };
            offset += 1;
            match byte {
                _ if is_escaped => is_escaped = false,
                b'/' if !is_in_class => break,
                b'[' => is_in_class = true,
                b']' => is_in_class = false,
                b'\\' => is_escaped = true,
                _ => {}
            }
        }
        self.token.value = offset as u32;
        offset = self.skip_name(offset);
        while !matches!(self.byte(offset), b'\\' | WIDE_UNIT)
            && chars::continues_name(self.byte(offset))
        {
            offset = self.skip_name(offset + 1);
        }
        if self.byte(offset) == WIDE_UNIT && self.source.text.at(offset) > 0xff {
            self.error = Some(LexicalError::WideRegExpFlag(start as u32, offset as u32));
            return self.set(TokenKind::Invalid, start, offset);
        }
        self.set(TokenKind::RegExp, start, offset);
    }

    /// The value of a string that is compared with other names.
    pub(crate) fn string_name(&mut self, token: Token) -> Name {
        let (start, end) = (token.start as usize + 1, token.end as usize - 1);
        if !token.is_elaborate {
            return self.names.intern_source(self.source, start, end);
        }
        self.unescape(start, end);
        (self.names).intern_text(self.source, Text::Utf16(&self.scratch))
    }

    #[inline]
    pub(crate) fn string_value(&mut self, token: Token) -> Name {
        self.literal(token, token.start as usize + 1, token.end as usize - 1)
    }

    #[inline]
    fn literal(&mut self, token: Token, start: usize, end: usize) -> Name {
        if !token.is_elaborate {
            return self.names.literal_in_source(self.source, start, end);
        }
        self.unescape(start, end);
        self.names.literal(self.source, Text::Utf16(&self.scratch))
    }

    fn template_range(token: Token) -> (usize, usize) {
        let delimiter = if token.value & TEMPLATE_IS_TAIL != 0 {
            1
        } else {
            2
        };
        (token.start as usize + 1, token.end as usize - delimiter)
    }

    /// TV of ECMA-262.
    pub(crate) fn cooked_template(&mut self, token: Token) -> Option<Name> {
        let (start, end) = Self::template_range(token);
        (token.value & TEMPLATE_IS_MALFORMED == 0).then(|| self.literal(token, start, end))
    }

    /// TRV of ECMA-262.
    pub(crate) fn raw_template(&mut self, token: Token) -> Name {
        let (mut offset, end) = Self::template_range(token);
        if !token.is_elaborate {
            return self.names.literal_in_source(self.source, offset, end);
        }
        self.scratch.clear();
        while offset < end {
            offset = self.push_normalized(offset);
        }
        self.names.literal(self.source, Text::Utf16(&self.scratch))
    }

    fn unit(&self, offset: usize) -> u16 {
        self.source.text.get(offset).unwrap_or(0)
    }

    /// Both CR and CR LF are LF.
    fn push_normalized(&mut self, offset: usize) -> usize {
        let unit = self.unit(offset);
        if unit != 0x0d {
            self.scratch.push(unit);
            return offset + 1;
        }
        self.scratch.push(0x0a);
        offset + 1 + usize::from(self.unit(offset + 1) == 0x0a)
    }

    /// Into `scratch`. The escapes are well formed.
    fn unescape(&mut self, mut offset: usize, end: usize) {
        self.scratch.clear();
        while offset < end {
            let rest = &self.bytes[offset..end];
            let plain = bun_highway::index_of_any_char(rest, b"\\\r").unwrap_or(rest.len());
            match self.source.text.slice(offset..offset + plain) {
                Text::Latin1(run) => self.scratch.extend(run.iter().map(|&it| u16::from(it))),
                Text::Utf16(run) => self.scratch.extend_from_slice(run),
            }
            offset += plain;
            match rest.get(plain) {
                Some(b'\\') => offset = self.push_escaped(offset + 1),
                Some(_) => offset = self.push_normalized(offset),
                None => {}
            }
        }
    }

    /// `offset` is past the backslash.
    fn push_escaped(&mut self, offset: usize) -> usize {
        let next = offset + 1;
        let digit = |offset: usize, radix| char::from(self.byte(offset)).to_digit(radix);
        let (value, end) = match self.byte(offset) {
            b'b' => (0x08, next),
            b't' => (0x09, next),
            b'n' => (0x0a, next),
            b'v' => (0x0b, next),
            b'f' => (0x0c, next),
            b'r' => (0x0d, next),
            b'\r' => return next + usize::from(self.unit(next) == 0x0a),
            b'\n' => return next,
            b'x' => {
                let (high, low) = (digit(next, 16), digit(next + 1, 16));
                (high.unwrap_or(0) << 4 | low.unwrap_or(0), next + 2)
            }
            b'u' => {
                let (code_point, end) = self.unicode_escape(next);
                (code_point.unwrap_or(0), end)
            }
            first @ b'0'..=b'7' => {
                let limit = offset + if first <= b'3' { 3 } else { 2 };
                let (mut value, mut end) = (0, offset);
                while end < limit
                    && let Some(digit) = digit(end, 8)
                {
                    value = value * 8 + digit;
                    end += 1;
                }
                (value, end)
            }
            _ => (u32::from(self.unit(offset)), next),
        };
        self.push_code_point(value);
        end
    }
}
