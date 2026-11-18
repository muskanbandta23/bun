//! What `indexOf`, `lastIndexOf` and `includes` do with dense elements.

use bun_highway::elements::{
    index_of_either_u64, index_of_f64, index_of_u64, last_index_of_either_u64, last_index_of_f64,
    last_index_of_u64,
};

use crate::{Elements, Gc, JsBigInt, JsString, Value};

#[derive(Clone, Copy, PartialEq, Eq, core::marker::ConstParamTy)]
pub enum Direction {
    Forward,
    Backward,
}

pub enum SearchResult {
    Found(usize),
    NotFound,
    /// The characters of the string at that index have to be put together before the search can go past it.
    Unresolved(usize),
}

impl From<Option<usize>> for SearchResult {
    #[inline(always)]
    fn from(index: Option<usize>) -> SearchResult {
        index.map_or(SearchResult::NotFound, SearchResult::Found)
    }
}

#[inline(always)]
fn find_word<const DIRECTION: Direction>(words: &[u64], word: u64) -> Option<usize> {
    match DIRECTION {
        Direction::Forward => index_of_u64(words, word),
        Direction::Backward => last_index_of_u64(words, word),
    }
}

#[inline(always)]
fn find_either<const DIRECTION: Direction>(words: &[u64], a: u64, b: u64) -> Option<usize> {
    match DIRECTION {
        Direction::Forward => index_of_either_u64(words, a, b),
        Direction::Backward => last_index_of_either_u64(words, a, b),
    }
}

fn find_zero<const DIRECTION: Direction>(words: &[u64]) -> Option<usize> {
    let [int32, positive, negative] =
        [Value::ZERO, Value::double(0.0), Value::double(-0.0)].map(Value::to_bits);
    let found = find_either::<DIRECTION>(words, int32, positive);
    let passed = match DIRECTION {
        Direction::Forward => 0..found.unwrap_or(words.len()),
        Direction::Backward => found.map_or(0, |index| index + 1)..words.len(),
    };
    let start = passed.start;
    (find_word::<DIRECTION>(&words[passed], negative).map(|index| start + index)).or(found)
}

#[inline(always)]
fn position<const DIRECTION: Direction>(
    values: &[Option<Value>],
    mut test: impl FnMut(Value) -> bool,
) -> Option<usize> {
    let test = |value: &Option<Value>| value.is_some_and(&mut test);
    match DIRECTION {
        Direction::Forward => values.iter().position(test),
        Direction::Backward => values.iter().rposition(test),
    }
}

/// `needle` is no concatenation that has yet to be resolved.
fn find_string<const DIRECTION: Direction>(
    values: &[Option<Value>],
    needle: Gc<JsString>,
) -> SearchResult {
    let view = needle.try_view().expect("resolved");
    let (text, length, is_atom) = (view.text(), needle.len(), needle.is_atom());
    let mut is_unresolved = false;
    let index = position::<DIRECTION>(values, |value| {
        if value == needle.into() {
            return true;
        }
        let Some(string) = value.as_string() else {
            return false;
        };
        if is_atom && string.is_atom() {
            return string.fiber() == needle.fiber();
        }
        if string.len() != length {
            return false;
        }
        match string.try_view() {
            Some(view) => view.text() == text,
            None => {
                is_unresolved = true;
                true
            }
        }
    });
    match index {
        Some(index) if is_unresolved => SearchResult::Unresolved(index),
        index => index.into(),
    }
}

fn find_bigint<const DIRECTION: Direction>(
    values: &[Option<Value>],
    needle: Gc<JsBigInt>,
) -> Option<usize> {
    position::<DIRECTION>(values, |value| {
        (value.as_bigint()).is_some_and(|bigint| bigint.value() == needle.value())
    })
}

impl Elements<'_> {
    /// By IsStrictlyEqual. If `needle` is a string, it has a view.
    #[inline]
    pub fn find_strictly_equal<const DIRECTION: Direction>(self, needle: Value) -> SearchResult {
        let words = self.words();
        match (self, needle.as_number()) {
            (Elements::Int32(_), Some(number)) => {
                let int32 = Value::number(number + 0.0);
                (int32.is_int32())
                    .then(|| find_word::<DIRECTION>(words, int32.to_bits()))
                    .flatten()
            }
            (Elements::Double(numbers), Some(number)) => match DIRECTION {
                Direction::Forward => index_of_f64(numbers, number),
                Direction::Backward => last_index_of_f64(numbers, number),
            },
            (Elements::Int32(_) | Elements::Double(_), None) => None,
            (Elements::Contiguous(_), Some(number)) => {
                if number == 0.0 {
                    find_zero::<DIRECTION>(words)
                } else if number.is_nan() {
                    None
                } else {
                    let (a, b) = (Value::number(number), Value::double(number));
                    find_either::<DIRECTION>(words, a.to_bits(), b.to_bits())
                }
            }
            (Elements::Contiguous(values), None) => {
                if let Some(string) = needle.as_string() {
                    return find_string::<DIRECTION>(values, string);
                }
                match needle.as_bigint() {
                    Some(bigint) => find_bigint::<DIRECTION>(values, bigint),
                    None => find_word::<DIRECTION>(words, needle.to_bits()),
                }
            }
        }
        .into()
    }

    /// By SameValueZero, with holes as undefined. If `needle` is a string, it has a view.
    #[inline]
    pub fn find_same_value_zero(self, needle: Value) -> SearchResult {
        match self {
            Elements::Contiguous(_) if needle.is_undefined() => {
                index_of_either_u64(self.words(), 0, needle.to_bits()).into()
            }
            _ if needle.is_undefined() => self.first_hole().into(),
            Elements::Contiguous(values) if needle.as_double().is_some_and(f64::is_nan) => {
                let is_nan = |value: Value| value.as_double().is_some_and(f64::is_nan);
                position::<{ Direction::Forward }>(values, is_nan).into()
            }
            _ => self.find_strictly_equal::<{ Direction::Forward }>(needle),
        }
    }
}
