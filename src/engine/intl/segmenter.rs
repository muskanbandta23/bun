//! `Intl.Segmenter`

use core::cell::{OnceCell, RefCell};
use core::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use bun_engine_text::{CodeUnit, StringRef, Text, with_text};
use bun_icu::break_iterator::{BreakIterator, BreakRules, Kind, MAX_TEXT_LENGTH};

use crate::error::IcuResult;
use crate::locale_resolver::ResolvedLocale;
use crate::{AvailableLocales, Cache, Error, LanguageTag, Result};

option_enum! {
    #[derive(Default)]
    pub enum Granularity: "granularity must be either \"grapheme\", \"word\", or \"sentence\"" {
        #[default]
        Grapheme = "grapheme",
        Word = "word",
        Sentence = "sentence",
    }
}

const ESTIMATED_ITERATOR_SIZE: usize = 2048;

/// Those with the same locale and granularity are one.
#[derive(Clone)]
pub struct Segmenter(Rc<Shared>);

struct Shared {
    locale: ResolvedLocale,
    granularity: Granularity,
    rules: OnceCell<BreakRules>,
}

impl Segmenter {
    pub fn new(
        cache: &Cache,
        requested: &[LanguageTag],
        granularity: Granularity,
    ) -> Result<Segmenter> {
        let locale = ResolvedLocale::new(cache, AvailableLocales::general(), requested)?;
        cache.segmenters.find_or_insert(
            |it| it.0.granularity == granularity && it.0.locale == locale,
            || {
                cache.note_allocation(ESTIMATED_ITERATOR_SIZE);
                Ok(Segmenter(Rc::new(Shared {
                    locale: locale.clone(),
                    granularity,
                    rules: OnceCell::new(),
                })))
            },
        )
    }

    pub fn locale(&self) -> &LanguageTag {
        self.0.locale.tag()
    }

    pub fn granularity(&self) -> Granularity {
        self.0.granularity
    }

    pub fn segment(&self, string: StringRef) -> Result<Segments> {
        if string.len() > MAX_TEXT_LENGTH {
            return Err(Error::OutOfMemory);
        }
        Ok(Segments {
            input: Rc::new(Input {
                segmenter: self.clone(),
                string,
                units: OnceCell::new(),
            }),
            iterator: RefCell::new(None),
        })
    }

    fn rules(&self) -> Result<&BreakRules> {
        self.0.rules.get_or_try_init(|| {
            let kind = match self.0.granularity {
                Granularity::Grapheme => Kind::Grapheme,
                Granularity::Word => Kind::Word,
                Granularity::Sentence => Kind::Sentence,
            };
            BreakRules::open(kind, &self.0.locale.icu_locale(&[])?)
                .or_type_error("failed to initialize Segmenter")
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Segment {
    pub range: Range<usize>,
    /// Of `Granularity::Word`.
    pub is_word_like: Option<bool>,
}

struct Input {
    segmenter: Segmenter,
    string: StringRef,
    /// The copy that break iterators read.
    units: OnceCell<Arc<Vec<u16>>>,
}

impl Input {
    fn iterator<'a>(
        &self,
        slot: &'a mut Option<BreakIterator>,
        failure: &'static str,
    ) -> Result<&'a mut BreakIterator> {
        if let Some(iterator) = slot {
            return Ok(iterator);
        }
        let units = self.units.get_or_try_init(|| {
            let mut units = Vec::new();
            (units.try_reserve_exact(self.string.len())).map_err(|_| Error::OutOfMemory)?;
            with_text!(self.string.text(), |text| units
                .extend(text.iter().map(|unit| unit.to_u16())));
            Ok::<_, Error>(Arc::new(units))
        })?;
        let iterator = self.segmenter.rules()?.iterate(Arc::clone(units));
        Ok(slot.insert(iterator.or_type_error(failure)?))
    }

    fn segment(&self, range: Range<usize>, at_end: &mut BreakIterator) -> Segment {
        let is_word = self.segmenter.granularity() == Granularity::Word;
        Segment {
            range,
            is_word_like: is_word.then(|| at_end.follows_word()),
        }
    }

    fn is_plain(&self) -> bool {
        self.segmenter.granularity() == Granularity::Grapheme && self.string.is_latin1()
    }

    /// Whether the units on both sides of `index` tell that it is a boundary.
    fn is_plain_boundary(&self, index: usize) -> bool {
        self.segmenter.granularity() == Granularity::Grapheme
            && is_plain_grapheme_boundary(self.string.text(), index)
    }
}

fn is_plain_grapheme_boundary(text: Text<'_>, index: usize) -> bool {
    let before = index.checked_sub(1).and_then(|index| text.get(index));
    match (before, text.get(index)) {
        (Some(0x0d), Some(0x0a)) => false,
        (Some(before), Some(after)) => stands_alone(before) && stands_alone(after),
        _ => true,
    }
}

/// No rule of UAX #29 but the one for CR LF joins two of these.
fn stands_alone(unit: u16) -> bool {
    matches!(
        unit,
        0..0x300 | 0x370..0x483 | 0x48a..0x591 | 0x3041..0x3099 | 0x309b..0xa000 | 0xac00..0xd7a4
    )
}

/// The result of `segment`.
pub struct Segments {
    input: Rc<Input>,
    iterator: RefCell<Option<BreakIterator>>,
}

impl Segments {
    /// `index` is a result of ToIntegerOrInfinity.
    pub fn containing(&self, index: f64) -> Result<Option<Segment>> {
        let length = self.input.string.len();
        if !(0.0..length as f64).contains(&index) {
            return Ok(None);
        }
        let index = index as usize;
        if self.input.is_plain_boundary(index) && self.input.is_plain_boundary(index + 1) {
            return Ok(Some(Segment {
                range: index..index + 1,
                is_word_like: None,
            }));
        }
        let mut iterator = self.iterator.borrow_mut();
        let iterator = (self.input).iterator(&mut iterator, "failed to initialize Segments")?;
        iterator.following(index);
        let start = iterator.previous().unwrap_or(0);
        let end = iterator.next().unwrap_or(length);
        Ok(Some(self.input.segment(start..end, iterator)))
    }

    /// Of what it can come to own.
    pub fn estimated_size(&self) -> usize {
        match self.input.is_plain() {
            true => 0,
            false => self.input.string.len() * size_of::<u16>(),
        }
    }

    pub fn iterate(&self) -> SegmentIterator {
        SegmentIterator {
            input: Rc::clone(&self.input),
            state: RefCell::default(),
        }
    }
}

pub struct SegmentIterator {
    input: Rc<Input>,
    state: RefCell<IteratorState>,
}

#[derive(Default)]
struct IteratorState {
    position: usize,
    iterator: Option<BreakIterator>,
    /// Where `iterator` stands.
    iterator_position: usize,
}

impl SegmentIterator {
    /// Of what it can come to own.
    pub fn estimated_size(&self) -> usize {
        match self.input.is_plain() {
            true => 0,
            false => ESTIMATED_ITERATOR_SIZE,
        }
    }

    pub fn next(&self) -> Result<Option<Segment>> {
        let state = &mut *self.state.borrow_mut();
        let (start, length) = (state.position, self.input.string.len());
        if start >= length {
            state.iterator = None;
            return Ok(None);
        }
        if self.input.is_plain_boundary(start + 1) {
            state.position += 1;
            return Ok(Some(Segment {
                range: start..start + 1,
                is_word_like: None,
            }));
        }
        let failure = "failed to initialize SegmentIterator";
        let iterator = self.input.iterator(&mut state.iterator, failure)?;
        let end = if state.iterator_position == start {
            iterator.next()
        } else {
            iterator.following(start)
        };
        let end = end.unwrap_or(length);
        (state.position, state.iterator_position) = (end, end);
        Ok(Some(self.input.segment(start..end, iterator)))
    }
}
