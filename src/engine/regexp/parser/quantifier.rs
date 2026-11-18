use bun_engine_text::{CodeUnit, MAX_LENGTH};

use super::{Parser, Quantifier, Sink, Token, token};
use crate::Error;
use crate::tree::UNBOUNDED;

/// What is before a quantifier.
#[derive(Clone, Copy)]
pub(super) enum Repeated {
    Atom,
    Lookbehind,
    Nothing,
}

impl<C: CodeUnit, S: Sink> Parser<'_, C, S> {
    #[inline(always)]
    pub(super) fn quantify(&mut self) -> Result<(), Error> {
        match self.quantifier(Repeated::Atom)? {
            Some(quantifier) => self.sink.quantify(quantifier),
            None => Ok(()),
        }
    }

    #[inline(always)]
    pub(super) fn quantifier(&mut self, repeated: Repeated) -> Result<Option<Quantifier>, Error> {
        let next = self.peek();
        if token(next) != Token::Quantifier {
            return Ok(None);
        }
        let (min, max) = match next {
            b'*' => (0, UNBOUNDED),
            b'+' => (1, UNBOUNDED),
            b'?' => (0, 1),
            _ => match self.counts()? {
                Some(counts) => counts,
                None => return Ok(None),
            },
        };
        self.at += 1;
        match repeated {
            Repeated::Atom => {}
            Repeated::Lookbehind => return Err(Error::QuantifiedLookbehind),
            Repeated::Nothing => return Err(Error::NothingToRepeat),
        }
        let greedy = !self.eat(b'?') || min == max;
        Ok(Some(Quantifier { min, max, greedy }))
    }

    /// From the left brace up to the right one, or nowhere if the brace stands for itself.
    fn counts(&mut self) -> Result<Option<(u32, u32)>, Error> {
        let start = self.at;
        self.at += 1;
        if self.peek().is_ascii_digit() {
            let min = self.decimal();
            let max = match self.eat(b',') {
                true if self.peek().is_ascii_digit() => self.decimal(),
                true => u64::MAX,
                false => min,
            };
            if self.peek() == b'}' {
                if min == u64::MAX {
                    return Err(Error::QuantifierTooLarge);
                }
                if min > max {
                    return Err(Error::QuantifierOutOfOrder);
                }
                self.sum_of_counts = self.sum_of_counts.saturating_add(min);
                // An iteration beyond the minimum consumes a character.
                let is_unbounded = max - min >= MAX_LENGTH as u64;
                let clamp = |count| u32::try_from(count).unwrap_or(UNBOUNDED);
                let max = if is_unbounded { UNBOUNDED } else { clamp(max) };
                return Ok(Some((clamp(min), max)));
            }
        }
        if self.is_unicode {
            return Err(Error::IncompleteQuantifier);
        }
        self.at = start;
        Ok(None)
    }
}
