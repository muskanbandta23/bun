//! Matching of URLs, names and lines against what a frontend asks for.

use core::cell::OnceCell;

use bun_engine_regexp::{Context, Flags, Mode, Options, Outcome, Pattern, Program, Width, compile};
use bun_engine_text::{StringBuilder, StringRef, Text};
use bun_engine_vm::Vm;

use crate::protocol::{Object, object};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    RegExp,
    Whole,
    Part,
}

/// One that is no valid pattern matches nothing.
pub(crate) struct Matcher {
    pattern: Option<Pattern>,
    latin1: OnceCell<Option<Program>>,
    utf16: OnceCell<Option<Program>>,
}

fn escaped(before: &str, text: Text<'_>, after: &str) -> StringBuilder {
    let mut pattern = StringBuilder::new();
    pattern.push_ascii(before);
    for unit in text.code_units() {
        if u8::try_from(unit).is_ok_and(|it| b"[](){}+-*.,?\\^$|".contains(&it)) {
            pattern.push_latin1(b'\\');
        }
        pattern.push_unit(unit);
    }
    pattern.push_ascii(after);
    pattern
}

impl Matcher {
    pub(crate) fn new(text: Text<'_>, syntax: Syntax, is_case_sensitive: bool) -> Matcher {
        let mut flags = Flags::empty();
        flags.set(Flags::IGNORE_CASE, !is_case_sensitive);
        let pattern = match syntax {
            Syntax::RegExp => Pattern::new(text, flags),
            Syntax::Whole => Pattern::new(escaped("^", text, "$").text(), flags),
            Syntax::Part => Pattern::new(escaped("", text, "").text(), flags),
        };
        Matcher {
            pattern: pattern.ok(),
            latin1: OnceCell::new(),
            utf16: OnceCell::new(),
        }
    }

    pub(crate) fn matches(&self, vm: &Vm, subject: Text<'_>) -> bool {
        self.matches_in(vm, subject, &mut Context::new())
    }

    fn matches_in(&self, vm: &Vm, subject: Text<'_>, context: &mut Context) -> bool {
        let (program, width) = match subject {
            Text::Latin1(_) => (&self.latin1, Width::Latin1),
            Text::Utf16(_) => (&self.utf16, Width::Utf16),
        };
        let program = program.get_or_init(|| {
            let options = Options {
                width,
                mode: Mode::MatchOnly,
                sample: Text::EMPTY,
                stack_limit: vm.hard_stack_limit(),
            };
            compile(self.pattern.as_ref()?.tree(), &options).ok()
        });
        program.as_ref().is_some_and(|program| {
            let outcome = program.exec(subject, 0, &mut [], context);
            matches!(outcome, Outcome::Match { .. })
        })
    }

    /// `GenericTypes.SearchMatch` for each. A line includes its line feed.
    pub(crate) fn matching_lines(&self, vm: &Vm, source: &StringRef) -> Vec<Object> {
        let (text, mut context) = (source.text(), Context::new());
        let (mut matches, mut start) = (Vec::new(), 0);
        for number in 0_u32.. {
            if start == text.len() {
                break;
            }
            let end = (text.find_unit(b'\n'.into(), start)).map_or(text.len(), |at| at + 1);
            if self.matches_in(vm, text.slice(start..end), &mut context) {
                let content = StringRef::substring(source, start..end);
                matches.push(object! { "lineNumber": number, "lineContent": content });
            }
            start = end;
        }
        matches
    }
}

/// What tells one request for blackboxing or for a symbolic breakpoint from another.
pub(crate) struct Query {
    text: StringRef,
    is_case_sensitive: bool,
    is_regexp: bool,
    matcher: Matcher,
}

impl Query {
    pub(crate) fn new(
        text: StringRef,
        is_case_sensitive: Option<bool>,
        is_regexp: Option<bool>,
    ) -> Query {
        let is_case_sensitive = is_case_sensitive.unwrap_or(true);
        let is_regexp = is_regexp.unwrap_or(false);
        let syntax = if is_regexp {
            Syntax::RegExp
        } else {
            Syntax::Whole
        };
        Query {
            matcher: Matcher::new(text.text(), syntax, is_case_sensitive),
            text,
            is_case_sensitive,
            is_regexp,
        }
    }

    /// Nothing matches an empty text.
    pub(crate) fn matches(&self, vm: &Vm, subject: Text<'_>) -> bool {
        if self.is_case_sensitive && !self.is_regexp {
            return !subject.is_empty() && self.text.text() == subject;
        }
        !subject.is_empty() && self.matcher.matches(vm, subject)
    }
}

impl PartialEq for Query {
    fn eq(&self, other: &Query) -> bool {
        self.text == other.text
            && self.is_case_sensitive == other.is_case_sensitive
            && self.is_regexp == other.is_regexp
    }
}
