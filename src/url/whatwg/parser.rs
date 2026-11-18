//! The basic URL parser, up to where the path starts.

use crate::class::{self, UPPERCASE};
use crate::components::{Components, State};
use crate::input::Source;
use crate::output::Output;
use crate::path::{is_normalized_drive_letter, starts_with_drive_letter};
use crate::record::View;
use crate::scheme::scheme_end;
use crate::{Error, Punycode, Result, Scheme};

/// What is left to read once nothing but a tab or a newline can make the parser fail.
#[derive(Clone, Copy)]
pub(crate) enum Tail {
    /// The index of the first segment to add. The output has a slash before it.
    Segments(usize),
    OpaquePath(usize),
    /// The index of a `?`, of a `#` or of the end.
    AfterPath(usize),
    /// The index of a `#` or of the end.
    AfterQuery(usize),
}

/// Fails on a tab or a newline, for the caller to take them out and start over.
pub(crate) struct Parser<'a> {
    pub(crate) source: Source<'a>,
    pub(crate) punycode: Punycode,
    pub(crate) out: Output<'a>,
    pub(crate) parts: Components,
}

impl<'a> Parser<'a> {
    #[inline]
    pub(crate) fn new(source: Source<'a>, punycode: Punycode, parts: Components) -> Parser<'a> {
        Parser {
            source,
            punycode,
            out: Output::new(source.bytes),
            parts,
        }
    }

    #[inline]
    pub(crate) fn mark(&self, at: usize) -> u32 {
        self.out.position(at) as u32
    }

    #[inline]
    pub(crate) fn is_special(&self) -> bool {
        self.parts.scheme.is_special()
    }

    #[inline]
    pub(crate) fn is_slash(&self, at: usize) -> bool {
        match self.source.bytes.get(at) {
            Some(b'/') => true,
            Some(b'\\') => self.is_special(),
            _ => false,
        }
    }

    /// `base` is valid.
    pub(crate) fn head(&mut self, base: Option<&View<'a>>) -> Result<Tail> {
        let input = self.source.bytes;
        let Some(colon) = scheme_end(input) else {
            return self.no_scheme(base.ok_or(Error::Invalid)?);
        };
        let name = &input[..colon];
        let has_uppercase = name
            .iter()
            .any(|&byte| class::host_traits(byte) & UPPERCASE != 0);
        let scheme = if has_uppercase {
            Scheme::of_any_case(name)
        } else {
            Scheme::of(name)
        };
        self.parts.scheme = scheme;
        self.parts.scheme_end = colon as u32;
        self.parts.state = State::Hierarchical;

        let at = colon + 1;
        let has_two_slashes = input[at..].starts_with(b"//");
        match (scheme, base.filter(|base| base.parts.scheme == scheme)) {
            (Scheme::File, base) => self.file(at, base),
            (Scheme::Other, _) => {
                if has_uppercase {
                    let out = self.out.interrupt(0)?;
                    out.extend(name)?;
                    out.make_ascii_lowercase();
                    self.out.resume(colon);
                }
                if has_two_slashes {
                    return self.authority(at + 2);
                }
                self.parts.set_empty_authority(at as u32);
                if input[at..].starts_with(b"/.//") {
                    self.parts.path_start += 2;
                }
                if self.is_slash(at) {
                    return Ok(Tail::Segments(self.parts.path_start as usize + 1));
                }
                self.parts.state = State::OpaquePath;
                Ok(Tail::OpaquePath(at))
            }
            (_, Some(base)) if !has_two_slashes => self.relative(at, base),
            _ => self.special_authority(at),
        }
    }

    fn no_scheme(&mut self, base: &View<'a>) -> Result<Tail> {
        if base.has_opaque_path() {
            return match self.source.bytes.first() {
                Some(b'#') => Ok(self.keep_query(0, base)),
                _ => Err(Error::Invalid),
            };
        }
        match base.parts.scheme {
            Scheme::File => self.file(0, Some(base)),
            _ => self.relative(0, base),
        }
    }

    /// Makes the output the scheme, which is special, and two slashes where the input has got to `at`.
    fn special_prefix(&mut self, at: usize) {
        let prefix = self.parts.scheme.prefix();
        self.parts.scheme_end = (prefix.len() - "://".len()) as u32;
        if self.source.bytes[..at] != *prefix {
            self.out.start_with(prefix, at);
        }
    }

    fn special_authority(&mut self, mut at: usize) -> Result<Tail> {
        while self.is_slash(at) {
            at += 1;
        }
        self.special_prefix(at);
        self.authority(at)
    }

    /// Takes the first `length` bytes of `base` for the input before `at`.
    fn inherit(&mut self, base: &View<'a>, length: u32, at: usize) {
        self.parts = base.parts;
        self.out.start_with(&base.text[..length as usize], at);
    }

    fn keep_query(&mut self, at: usize, base: &View<'a>) -> Tail {
        self.inherit(base, base.parts.query_end, at);
        Tail::AfterQuery(at)
    }

    fn relative(&mut self, at: usize, base: &View<'a>) -> Result<Tail> {
        self.parts = base.parts;
        if !self.is_slash(at) {
            return self.keep_path(at, base);
        }
        if !self.is_slash(at + 1) {
            self.inherit(base, base.parts.authority_end(), at);
            return self.path_start(at);
        }
        if self.is_special() {
            return self.special_authority(at);
        }
        self.inherit(base, base.parts.scheme_end + 1, at);
        self.authority(at + 2)
    }

    /// For input that has no slash at `at`.
    fn keep_path(&mut self, at: usize, base: &View<'a>) -> Result<Tail> {
        match self.source.bytes.get(at) {
            None | Some(b'#') => return Ok(self.keep_query(at, base)),
            Some(b'?') => {
                self.inherit(base, base.parts.path_end, at);
                return Ok(Tail::AfterPath(at));
            }
            Some(_) => {}
        }
        let path = base.path();
        let is_file = base.parts.scheme == Scheme::File;
        if is_file && starts_with_drive_letter(&self.source.bytes[at..]) {
            self.inherit(base, base.parts.path_start + 1, at);
        } else if path.is_empty() || (is_file && is_normalized_drive_letter(&path[1..])) {
            self.inherit(base, base.parts.path_end, at);
            self.out.replace(at, at, b"/")?;
        } else {
            let last_slash = bun_highway::last_index_of_slash(path).unwrap_or(0);
            self.inherit(base, base.parts.path_start + last_slash as u32 + 1, at);
        }
        Ok(Tail::Segments(at))
    }

    /// `base` is a file URL.
    fn file(&mut self, at: usize, base: Option<&View<'a>>) -> Result<Tail> {
        self.parts.scheme = Scheme::File;
        self.parts.state = State::Hierarchical;
        if self.is_slash(at) && self.is_slash(at + 1) {
            return self.file_host(at + 2);
        }
        let Some(base) = base else {
            self.special_prefix(at);
            self.parts.set_empty_authority(self.mark(at));
            return self.path_start(at);
        };
        if !self.is_slash(at) {
            return self.keep_path(at, base);
        }
        let keeps_drive = !starts_with_drive_letter(&self.source.bytes[at + 1..])
            && matches!(base.path(), [b'/', letter, b':'] | [b'/', letter, b':', b'/', ..] if letter.is_ascii_alphabetic());
        let drive_length = if keeps_drive { "/c:".len() as u32 } else { 0 };
        self.inherit(base, base.parts.path_start + drive_length, at);
        self.first_slash(at)
    }

    fn file_host(&mut self, start: usize) -> Result<Tail> {
        self.special_prefix(start);
        self.parts.set_empty_authority(self.mark(start));
        if starts_with_drive_letter(&self.source.bytes[start..]) {
            return self.path_start(start);
        }
        let (end, traits) = self.scan_host(start);
        self.host_of_file(start, end, traits)?;
        self.parts.host_end = self.mark(end);
        self.path_start(end)
    }

    pub(crate) fn path_start(&mut self, at: usize) -> Result<Tail> {
        self.parts.path_start = self.mark(at);
        if !self.is_special() && !self.is_slash(at) {
            return Ok(Tail::AfterPath(at));
        }
        self.first_slash(at)
    }

    /// Sees to it that the output has a `/` here.
    pub(crate) fn first_slash(&mut self, at: usize) -> Result<Tail> {
        if self.source.bytes.get(at) == Some(&b'/') {
            return Ok(Tail::Segments(at + 1));
        }
        let segments = at + usize::from(self.is_slash(at));
        self.out.replace(at, segments, b"/")?;
        Ok(Tail::Segments(segments))
    }
}
