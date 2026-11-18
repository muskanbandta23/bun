//! Noting errors, and going back to try another way.

use bun_engine_text::{StringBuilder, StringRef};

use super::{Facts, Session};
use crate::ast::OpenList;
use crate::error::{Complaint, Error, ErrorKind, LexicalError, Part, Recovery};
use crate::names::Name;
use crate::scope;
use crate::token::{Token, TokenKind};

/// The first complaint is the one that counts. There may be none, for the caller knows best what was expected.
pub(super) struct Failed;

pub(super) type Parsed<T> = Result<T, Failed>;

/// A place to go back to.
#[derive(Clone, Copy)]
pub(super) struct Mark {
    pub(super) token: Token,
    previous_end: u32,
    facts: Facts,
    assignments: u32,
    pauses: u32,
    list: OpenList,
    scopes: scope::Mark,
}

/// Where the first way to read something ended.
pub(super) struct Attempt {
    token: Token,
    previous_end: u32,
    complaint: Option<Complaint>,
    lexical_error: Option<LexicalError>,
}

const NO_PARTS: [Part; 3] = [Part::NONE; 3];

impl Session<'_> {
    pub(super) fn complain(&mut self, complaint: Complaint) -> Failed {
        self.complaint.get_or_insert(complaint);
        Failed
    }

    #[cold]
    #[inline(never)]
    pub(super) fn unexpected(&mut self) -> Failed {
        self.complain(Complaint::Unexpected(self.token()))
    }

    /// The token is not what was expected.
    #[cold]
    #[inline(never)]
    pub(super) fn fail_with(&mut self, sentence: &'static str, parts: [Part; 3]) -> Failed {
        if self.kind().speaks_for_itself() {
            return self.unexpected();
        }
        self.complain(Complaint::Explained(self.token(), sentence, parts))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fail(&mut self, sentence: &'static str) -> Failed {
        self.fail_with(sentence, NO_PARTS)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fail_naming(&mut self, sentence: &'static str, name: Name) -> Failed {
        self.fail_with(sentence, [name.into(), Part::NONE, Part::NONE])
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fail_saying(&mut self, sentence: &'static str, text: &'static str) -> Failed {
        self.fail_with(sentence, [text.into(), Part::NONE, Part::NONE])
    }

    /// The grammar allows what a rule does not.
    #[cold]
    #[inline(never)]
    pub(super) fn reject_with(&mut self, sentence: &'static str, parts: [Part; 3]) -> Failed {
        self.complain(Complaint::Sentence(sentence, parts))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn reject(&mut self, sentence: &'static str) -> Failed {
        self.reject_with(sentence, NO_PARTS)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn reject_naming(&mut self, sentence: &'static str, name: Name) -> Failed {
        self.reject_with(sentence, [name.into(), Part::NONE, Part::NONE])
    }

    #[cold]
    #[inline(never)]
    pub(super) fn reject_saying(&mut self, sentence: &'static str, text: &'static str) -> Failed {
        self.reject_with(sentence, [text.into(), Part::NONE, Part::NONE])
    }

    #[cold]
    #[inline(never)]
    pub(super) fn overflow(&mut self) -> Failed {
        self.has_overflowed = true;
        self.complain(Complaint::Sentence("Stack exhausted", NO_PARTS))
    }

    #[inline(always)]
    pub(super) fn mark(&self) -> Mark {
        Mark {
            token: self.token(),
            previous_end: self.previous_end(),
            facts: self.facts,
            assignments: self.assignments,
            pauses: self.pauses.as_ref().map_or(0, |pauses| pauses.len() as u32),
            list: self.ast.open_list(),
            scopes: self.scopes.mark(),
        }
    }

    /// The context is not part of it: what changes the context changes it back, whether it fails or not.
    pub(super) fn rewind(&mut self, mark: &Mark) {
        self.units_read_again += u64::from(self.start().saturating_sub(mark.token.start));
        self.lexer.restore(mark.token, mark.previous_end);
        self.facts = mark.facts;
        self.assignments = mark.assignments;
        if let Some(pauses) = &mut self.pauses {
            pauses.truncate(mark.pauses as usize);
        }
        self.ast.abandon_list(mark.list);
        self.scopes.rewind(mark.scopes);
        self.complaint = None;
        self.is_hopeless = false;
    }

    /// Whether to try another way after a failure. Ways within ways take exponential time.
    pub(super) fn may_try_again(&mut self) -> bool {
        let length = u64::from(self.lexer.end() - self.options.range.start.min(self.lexer.end()));
        if self.units_read_again > 64 * length + (1 << 20) {
            self.overflow();
        }
        let is_weary = self.units_read_again > 4 * length + (1 << 16);
        !(self.has_overflowed || (is_weary && self.is_hopeless))
    }

    /// Goes back to `mark`.
    pub(super) fn abandon_attempt(&mut self, mark: &Mark) -> Attempt {
        let attempt = self.attempt();
        self.rewind(mark);
        attempt
    }

    fn attempt(&self) -> Attempt {
        Attempt {
            token: self.token(),
            previous_end: self.previous_end(),
            complaint: self.complaint,
            lexical_error: self.lexer.error,
        }
    }

    /// The second way was no better, and the error is that of the first.
    pub(super) fn resume_attempt(&mut self, attempt: &Attempt) {
        if self.has_overflowed {
            return;
        }
        self.lexer.restore(attempt.token, attempt.previous_end);
        self.lexer.read_again();
        self.lexer.error = attempt.lexical_error;
        self.complaint = attempt.complaint;
    }

    /// Leaves things as a second way would have that is sure to fail.
    #[cold]
    #[inline(never)]
    pub(super) fn skip_attempt(&mut self) -> Failed {
        let attempt = self.attempt();
        self.resume_attempt(&attempt);
        Failed
    }

    #[cold]
    pub(super) fn error(&mut self) -> Error {
        self.eat(TokenKind::End);
        let token = self.token();
        let mut message = StringBuilder::new();
        match (self.lexer.error, self.complaint) {
            (Some(error), _) => error.write(&mut message, self.lexer.source),
            (None, Some(complaint)) => {
                complaint.write(&mut message, self.lexer.source, &self.lexer.names);
            }
            (None, None) => message.push_ascii("Parser error"),
        }
        let (kind, message) = match message.finish() {
            _ if self.has_overflowed => (ErrorKind::StackOverflow, StringRef::empty()),
            Ok(message) => (ErrorKind::Syntax, message),
            Err(_) => (ErrorKind::OutOfMemory, StringRef::empty()),
        };
        Error {
            kind,
            recovery: match token.kind {
                TokenKind::End | TokenKind::UnterminatedMultiline => Recovery::Recoverable,
                TokenKind::Unterminated | TokenKind::IncompleteEscape => {
                    Recovery::UnterminatedLiteral
                }
                _ => Recovery::Irrecoverable,
            },
            message,
            token: token.start..token.end,
            offset: token.end,
        }
    }
}
