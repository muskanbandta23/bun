//! The basic URL parser, from the path on.

use bun_highway::url::Scan;

use crate::class::{
    self, C0_CONTROL, FRAGMENT, NUMBER_SIGN, OPAQUE_PATH_SCAN, PATH, PERCENT, QUERY, QUESTION_MARK,
    SPECIAL_QUERY, Set, TAB_OR_NEWLINE, TAIL_SCAN,
};
use crate::parser::{Parser, Tail};
use crate::{Error, Result, Scheme, percent};

pub(crate) fn is_normalized_drive_letter(segment: &[u8]) -> bool {
    matches!(segment, [letter, b':'] if letter.is_ascii_alphabetic())
}

pub(crate) fn starts_with_drive_letter(text: &[u8]) -> bool {
    matches!(
        text,
        [letter, b':' | b'|'] | [letter, b':' | b'|', b'/' | b'\\' | b'?' | b'#', ..]
            if letter.is_ascii_alphabetic()
    )
}

/// The length of the dot that `text` starts with.
#[inline]
fn dot(text: &[u8], set: Set) -> Option<usize> {
    match text {
        [b'.', ..] => Some(1),
        [b'%', b'2', b'e' | b'E', ..] if set & PERCENT == 0 => Some(3),
        _ => None,
    }
}

/// How the bytes of a path are read.
#[derive(Clone, Copy)]
pub(crate) struct PathSyntax {
    /// What to encode. A `?`, a `#` or a `%` that is in it means nothing else.
    pub(crate) set: Set,
    pub(crate) backslash_is_slash: bool,
}

impl PathSyntax {
    #[inline]
    fn is_slash(self, byte: u8) -> bool {
        byte == b'/' || (byte == b'\\' && self.backslash_is_slash)
    }

    #[inline]
    fn ends_segment(self, byte: Option<&u8>) -> bool {
        byte.is_none_or(|&byte| {
            self.is_slash(byte) || class::is_in((QUESTION_MARK | NUMBER_SIGN) & !self.set, byte)
        })
    }

    /// The length of the dot segment that `text` starts with, and whether it has two dots.
    #[inline]
    pub(crate) fn dot_segment(self, text: &[u8]) -> Option<(usize, bool)> {
        let first = dot(text, self.set)?;
        let second = dot(&text[first..], self.set).unwrap_or(0);
        self.ends_segment(text.get(first + second))
            .then_some((first + second, second != 0))
    }
}

impl<'a> Parser<'a> {
    pub(crate) fn tail(&mut self, tail: Tail) -> Result<()> {
        let input = self.source.bytes;
        let (mut at, mut scan) = match tail {
            Tail::Segments(at) => {
                let mut scan = TAIL_SCAN.scan(input, at);
                (self.path(at, &mut scan, self.path_syntax(PATH))?, scan)
            }
            Tail::OpaquePath(at) => {
                let end = self.opaque_path(at)?;
                (end, TAIL_SCAN.scan(input, end + 1))
            }
            Tail::AfterPath(at) | Tail::AfterQuery(at) => (at, TAIL_SCAN.scan(input, at + 1)),
        };
        if !matches!(tail, Tail::AfterQuery(_)) {
            self.parts.path_end = self.mark(at);
        }
        if input.get(at) == Some(&b'?') {
            at = self.encode_up_to_fragment(&mut scan, self.query_set())?;
        }
        self.parts.query_end = self.mark(at);
        while at < input.len() {
            at = self.encode_up_to_fragment(&mut scan, FRAGMENT)?;
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn path_syntax(&self, set: Set) -> PathSyntax {
        PathSyntax {
            set,
            backslash_is_slash: self.is_special(),
        }
    }

    #[inline]
    pub(crate) fn query_set(&self) -> Set {
        if self.is_special() {
            SPECIAL_QUERY
        } else {
            QUERY
        }
    }

    #[inline]
    pub(crate) fn escape(&mut self, at: usize) -> Result<()> {
        let out = self.out.interrupt(at)?;
        percent::push_escaped(out, self.source.bytes[at], self.source.encoding)?;
        self.out.resume(at + 1);
        Ok(())
    }

    /// Escapes the byte at `start` and those of `set` that follow it. Returns the index behind them.
    fn escape_run(&mut self, start: usize, set: Set) -> Result<usize> {
        let out = self.out.interrupt(start)?;
        let mut at = start;
        for &byte in &self.source.bytes[start..] {
            if !class::is_in(set, byte) {
                break;
            }
            // In a file name, where a `%` is itself, these are too.
            if class::is_in(TAB_OR_NEWLINE, byte) && set & PERCENT == 0 {
                return Err(Error::Invalid);
            }
            percent::push_escaped(out, byte, self.source.encoding)?;
            at += 1;
        }
        self.out.resume(at);
        Ok(at)
    }

    /// Returns the index of the next `#` that is not in `set`, or of the end.
    #[inline]
    pub(crate) fn encode_up_to_fragment(&mut self, scan: &mut Scan<'a>, set: Set) -> Result<usize> {
        while let Some(at) = scan.next() {
            let byte = self.source.bytes[at];
            if class::is_in(set, byte) {
                scan.skip_to(self.escape_run(at, set)?);
            } else if byte == b'#' {
                return Ok(at);
            }
        }
        Ok(self.source.bytes.len())
    }

    /// Returns where the path ends, which `scan` has passed.
    pub(crate) fn path(
        &mut self,
        start: usize,
        scan: &mut Scan<'a>,
        syntax: PathSyntax,
    ) -> Result<usize> {
        let input = self.source.bytes;
        scan.skip_to(self.segment_start(start, syntax)?);
        let end = loop {
            let Some(at) = scan.next() else {
                break input.len();
            };
            let byte = input[at];
            if class::is_in(syntax.set, byte) {
                scan.skip_to(self.escape_run(at, syntax.set)?);
                continue;
            }
            match byte {
                b'?' | b'#' => break at,
                b'\\' if syntax.backslash_is_slash => {
                    self.out.replace(at, at + 1, b"/")?;
                    scan.skip_to(self.segment_start(at + 1, syntax)?);
                }
                b'.' | b'%' if input[at - 1] == b'/' => {
                    scan.skip_to(self.segment_start(at, syntax)?);
                }
                _ => {}
            }
        };
        if !self.parts.has_host() {
            self.tell_path_from_authority(end)?;
        }
        Ok(end)
    }

    /// Resolves the dot segments at `at`, and returns where the segment behind them starts.
    #[inline]
    fn segment_start(&mut self, mut at: usize, syntax: PathSyntax) -> Result<usize> {
        let input = self.source.bytes;
        while let Some((length, has_two_dots)) = syntax.dot_segment(&input[at..]) {
            self.out.interrupt(at)?;
            if has_two_dots {
                self.shorten_path(at);
            }
            at += length;
            at += usize::from(input.get(at).is_some_and(|&byte| syntax.is_slash(byte)));
            self.out.resume(at);
        }
        if self.parts.scheme == Scheme::File
            && let [letter, b'|', rest @ ..] = &input[at..]
            && letter.is_ascii_alphabetic()
            && !class::is_in(syntax.set, b'|')
            && syntax.ends_segment(rest.first())
            && self.mark(at) == self.parts.path_start + 1
        {
            self.out.replace(at + 1, at + 2, b":")?;
        }
        Ok(at)
    }

    /// The output is interrupted at `at`.
    fn shorten_path(&mut self, at: usize) {
        let start = self.parts.path_start as usize;
        let is_drive = self.parts.scheme == Scheme::File
            && self.out.position(at) - start == "/c:/".len()
            && is_normalized_drive_letter(&[self.out.byte(start + 1), self.out.byte(start + 2)]);
        if !is_drive {
            self.out.drop_last_segment(start);
        }
    }

    /// A path that starts with two slashes has `/.` before it if there is no host.
    fn tell_path_from_authority(&mut self, end: usize) -> Result<()> {
        let start = self.parts.path_start as usize;
        let is_ambiguous = self.out.position(end) - start >= 2 && self.out.byte(start + 1) == b'/';
        let is_guarded = self.parts.path_start != self.parts.host_end;
        if is_ambiguous && !is_guarded {
            self.out.flatten(end)?.insert(start, b"/.")?;
            self.parts.path_start += 2;
        } else if is_guarded && !is_ambiguous {
            self.out.flatten(end)?.remove(start - 2, 2);
            self.parts.path_start -= 2;
        }
        Ok(())
    }

    /// Returns where the path ends.
    fn opaque_path(&mut self, start: usize) -> Result<usize> {
        let input = self.source.bytes;
        let mut scan = OPAQUE_PATH_SCAN.scan(input, start);
        while let Some(at) = scan.next() {
            match input[at] {
                b'?' | b'#' => return Ok(at),
                b' ' if matches!(input.get(at + 1), Some(b'?' | b'#')) => self.escape(at)?,
                b' ' => {}
                _ => scan.skip_to(self.escape_run(at, C0_CONTROL)?),
            }
        }
        Ok(input.len())
    }
}
