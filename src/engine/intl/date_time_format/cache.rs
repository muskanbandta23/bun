use core::cell::RefCell;
use std::rc::Rc;

use bun_icu::date_pattern::{Field, Pattern, PatternGenerator, Width};
use bun_icu::{Buffer, CText};
use smallvec::SmallVec;

use super::writer::Writer;
use super::{DateTimeFormat, Options, Purpose};
use crate::recent::Recent;
use crate::{KeywordValue, LanguageTag};

/// The arguments that a format was made from.
struct Request {
    locales: SmallVec<[LanguageTag; 1]>,
    options: Options,
    purpose: Purpose,
}

#[derive(Default)]
pub(crate) struct DateCache {
    generators: Recent<CText, Rc<RefCell<PatternGenerator>>, 2>,
    writers: Recent<CText, Rc<Writer>, 4>,
    formats: Recent<Request, DateTimeFormat, 8>,
}

impl DateCache {
    fn generator(&self, locale: &CText) -> bun_icu::Result<Rc<RefCell<PatternGenerator>>> {
        (self.generators).find_or_make(locale, || {
            PatternGenerator::open(locale).map(|it| Rc::new(it.into()))
        })
    }

    pub(crate) fn best_pattern(
        &self,
        locale: &CText,
        skeleton: &[u16],
    ) -> bun_icu::Result<Pattern> {
        self.generator(locale)?.borrow_mut().best_pattern(skeleton)
    }

    pub(crate) fn field_name(
        &self,
        locale: &CText,
        field: Field,
        width: Width,
    ) -> bun_icu::Result<Buffer<u16>> {
        self.generator(locale)?.borrow().field_name(field, width)
    }

    pub(super) fn writer(&self, locale: &CText) -> bun_icu::Result<Rc<Writer>> {
        (self.writers).find_or_make(locale, || Writer::open(locale).map(Rc::new))
    }

    pub(super) fn find_format(
        &self,
        locales: &[LanguageTag],
        options: &Options,
        purpose: Purpose,
    ) -> Option<DateTimeFormat> {
        (self.formats)
            .find(|it| it.purpose == purpose && it.options == *options && *it.locales == *locales)
    }

    pub(super) fn keep_format(
        &self,
        locales: &[LanguageTag],
        options: &Options,
        purpose: Purpose,
        format: &DateTimeFormat,
    ) {
        const MAX_TEXT_LENGTH: usize = 128;

        let keywords = [&options.calendar, &options.numbering_system];
        let text_length: usize = (locales.iter().map(LanguageTag::as_bytes))
            .chain(keywords.into_iter().flatten().map(KeywordValue::as_bytes))
            .map(<[u8]>::len)
            .sum();
        if text_length <= MAX_TEXT_LENGTH {
            let request = Request {
                locales: locales.into(),
                options: options.clone(),
                purpose,
            };
            self.formats.insert(request, format.clone());
        }
    }
}
