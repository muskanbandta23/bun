//! One token at a time, to say what is wrong with a text and to check a primitive without making it.

use bun_engine_number::{JsonNumberError, parse_json_number};
use bun_engine_text::{CodeUnit, scan};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Token {
    /// A string, a number, `true`, `false` or `null`.
    Primitive,
    Identifier,
    Punctuator(u8),
    End,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Malformed {
    Fixed(&'static str),
    UnknownCharacter,
    UnknownEscape,
    /// Fewer than four units follow.
    ShortUnicodeEscape,
    BadUnicodeEscape,
}

pub(super) struct Lexed {
    pub(super) start: usize,
    /// Of a malformed token, where it went wrong.
    pub(super) end: usize,
    pub(super) token: Result<Token, Malformed>,
}

/// The token that `units` have at `at`, after white space.
pub(super) fn lex<T: CodeUnit>(units: &[T], at: usize) -> Lexed {
    let start = at + scan::json_white_space_len(&units[at..]);
    let (length, token) = lex_first(&units[start..]);
    Lexed {
        start,
        end: start + length,
        token,
    }
}

fn lex_first<T: CodeUnit>(units: &[T]) -> (usize, Result<Token, Malformed>) {
    let Some(first) = units.first() else {
        return (0, Ok(Token::End));
    };
    match u8::try_from(first.to_u16()).unwrap_or_default() {
        b'"' => lex_string(units),
        b'\'' => {
            let message = "Single quotes (') are not allowed in JSON";
            (0, Err(Malformed::Fixed(message)))
        }
        b'-' | b'0'..=b'9' => match parse_json_number(units) {
            Ok((_, length)) => (length, Ok(Token::Primitive)),
            Err(error) => {
                let digits = scan::run_len(&units[1..], |unit| unit.is_ascii_digit());
                let point = usize::from(error == JsonNumberError::NoDigitsAfterPoint);
                (1 + digits + point, Err(Malformed::Fixed(error.message())))
            }
        },
        b'a'..=b'z' | b'A'..=b'Z' | b'$' | b'_' => lex_word(units),
        punctuator @ (b'[' | b']' | b'{' | b'}' | b'(' | b')' | b':' | b',' | b'.' | b'='
        | b';') => (1, Ok(Token::Punctuator(punctuator))),
        _ => (0, Err(Malformed::UnknownCharacter)),
    }
}

fn lex_word<T: CodeUnit>(units: &[T]) -> (usize, Result<Token, Malformed>) {
    const ZERO_WIDTH_JOINER: u16 = 0x200d;
    for literal in ["true", "false", "null"] {
        let head = units.get(..literal.len());
        if head.is_some_and(|head| T::text(head).eq_ascii(literal)) {
            return (literal.len(), Ok(Token::Primitive));
        }
    }
    let length = scan::run_len(units, |unit| {
        unit.is_ascii_alphabetic()
            | unit.is_ascii_digit()
            | unit.is(b'$')
            | unit.is(b'_')
            | (unit | 1 == ZERO_WIDTH_JOINER)
    });
    (length, Ok(Token::Identifier))
}

fn lex_string<T: CodeUnit>(units: &[T]) -> (usize, Result<Token, Malformed>) {
    const UNTERMINATED: Malformed = Malformed::Fixed("Unterminated string");
    let mut at = 1;
    loop {
        at += scan::json_string_run_len(&units[at..]);
        match &units[at..] {
            [quote, ..] if quote.is(b'"') => return (at + 1, Ok(Token::Primitive)),
            [backslash] if backslash.is(b'\\') => return (at + 1, Err(UNTERMINATED)),
            [backslash, escape @ ..] if backslash.is(b'\\') => match unescape(escape) {
                Ok((_, length)) => at += 1 + length,
                Err(malformed) => return (at + 1, Err(malformed)),
            },
            _ => return (at, Err(UNTERMINATED)),
        }
    }
}

/// The unit that an escape stands for, and the length of the escape. `units` follow the backslash.
#[inline]
pub(super) fn unescape<T: CodeUnit>(units: &[T]) -> Result<(u16, usize), Malformed> {
    let letter = units.first().map_or(0, |letter| letter.to_u16());
    let unit = match u8::try_from(letter).unwrap_or_default() {
        itself @ (b'"' | b'\\' | b'/') => itself,
        b'b' => 0x08,
        b'f' => 0x0c,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'u' => {
            let digits = units.get(1..5).ok_or(Malformed::ShortUnicodeEscape)?;
            let value = (digits.iter())
                .try_fold(0, |value, digit| Some(value << 4 | digit.digit_value(16)?));
            return Ok((value.ok_or(Malformed::BadUnicodeEscape)? as u16, 5));
        }
        _ => return Err(Malformed::UnknownEscape),
    };
    Ok((u16::from(unit), 1))
}
