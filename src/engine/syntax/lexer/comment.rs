//! Comments, and the directives in them.

use super::Lexer;
use crate::chars::{self, Start};
use crate::token::TokenKind;

impl Lexer<'_> {
    /// Where the line ends.
    #[inline(never)]
    pub(super) fn skip_line(&mut self, mut offset: usize) -> usize {
        loop {
            offset = self.stop(offset, |stops| stops.in_string);
            match self.bytes.get(offset) {
                Some(&byte) if !chars::is_newline(byte) => offset = self.skip_stop(offset),
                _ => return offset,
            }
        }
    }

    /// Past a stop that means nothing here, and past the characters that are not ASCII after it.
    #[inline(always)]
    pub(super) fn skip_stop(&self, mut offset: usize) -> usize {
        offset += 1;
        while !self.byte(offset).is_ascii() {
            offset += 1;
        }
        offset
    }

    /// `offset` is past the slashes.
    #[inline(always)]
    pub(super) fn line_comment(&mut self, offset: usize) -> usize {
        if matches!(self.byte(offset), b'#' | b'@') {
            self.directive(offset + 1);
        }
        self.skip_line(offset)
    }

    /// Where it ends, if it does.
    #[inline(never)]
    pub(super) fn block_comment(&mut self, start: usize) -> Option<usize> {
        let rest = &self.bytes[start + 2..];
        let (text, end) = match bun_highway::memmem(rest, b"*/") {
            Some(length) => (&rest[..length], Some(start + length + 4)),
            None => (rest, None),
        };
        if !self.token.follows_newline {
            self.token.follows_newline = bun_highway::index_of_any_char(text, b"\n\r").is_some();
        }
        if end.is_none() {
            self.fail(
                TokenKind::UnterminatedMultiline,
                start,
                self.bytes.len(),
                "Multiline comment was not closed properly",
            );
        }
        end
    }

    /// Whether `-->` starts a comment here (ECMA-262 B.1.1).
    #[cold]
    pub(super) fn may_close_html_comment(&self) -> bool {
        !self.is_module && (self.token.follows_newline || self.previous_end == self.first_offset)
    }

    fn skip_blanks(&self, mut offset: usize) -> usize {
        while chars::start(self.byte(offset)) == Start::Blank {
            offset += 1;
        }
        offset
    }

    #[cold]
    #[inline(never)]
    fn directive(&mut self, offset: usize) {
        if chars::start(self.byte(offset)) != Start::Blank {
            return;
        }
        let rest = &self.bytes[offset + 1..];
        if let Some(value) = rest.strip_prefix(b"sourceURL=") {
            self.source_url = self.directive_value(self.bytes.len() - value.len());
        } else if let Some(value) = rest.strip_prefix(b"sourceMappingURL=") {
            self.source_mapping_url = self.directive_value(self.bytes.len() - value.len());
        }
    }

    fn directive_value(&self, offset: usize) -> Option<(u32, u32)> {
        let start = self.skip_blanks(offset);
        let rest = &self.bytes[start..];
        let length = bun_highway::index_of_any_char(rest, b"\t\n\x0b\x0c\r \"'\xa0");
        let end = start + length.unwrap_or(rest.len());
        let ends_line =
            (self.bytes.get(self.skip_blanks(end))).is_none_or(|&byte| chars::is_newline(byte));
        ends_line.then_some((start as u32, end as u32))
    }
}
