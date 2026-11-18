use core::cell::RefCell;
use std::rc::Rc;

use bun_icu::date_pattern::{Field, Pattern, PatternGenerator, Width};
use bun_icu::{Buffer, CText};

use crate::recent::Recent;

#[derive(Default)]
pub(crate) struct DateCache {
    generators: Recent<CText, Rc<RefCell<PatternGenerator>>, 2>,
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
}
