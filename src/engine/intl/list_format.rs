//! `Intl.ListFormat`

use core::cmp::Ordering;
use std::rc::Rc;

use bun_engine_text::{StringBuilder, StringRef};
use bun_icu::formatted_value::Category;
use bun_icu::list_format::{ELEMENT_FIELD, Kind, ListFormatter, Width};
use smallvec::SmallVec;

use crate::error::IcuResult;
use crate::locale_resolver::ResolvedLocale;
use crate::parts::new_string;
use crate::{AvailableLocales, Cache, Error, LanguageTag, PartKind, Result, Style};

option_enum! {
    #[derive(Default)]
    pub enum Type: "type must be either \"conjunction\", \"disjunction\", or \"unit\"" {
        #[default]
        Conjunction = "conjunction",
        Disjunction = "disjunction",
        Unit = "unit",
    }
}

/// Those with the same locale and options are one.
#[derive(Clone)]
pub struct ListFormat(Rc<Shared>);

struct Shared {
    locale: ResolvedLocale,
    kind: Type,
    style: Style,
    formatter: ListFormatter,
}

impl ListFormat {
    pub fn new(
        cache: &Cache,
        requested: &[LanguageTag],
        kind: Type,
        style: Style,
    ) -> Result<ListFormat> {
        let locale = ResolvedLocale::new(cache, AvailableLocales::general(), requested)?;
        cache.list_formats.find_or_insert(
            |it| it.0.kind == kind && it.0.style == style && it.0.locale == locale,
            || {
                let icu_kind = match kind {
                    Type::Conjunction => Kind::And,
                    Type::Disjunction => Kind::Or,
                    Type::Unit => Kind::Units,
                };
                let width = match style {
                    Style::Long => Width::Wide,
                    Style::Short => Width::Short,
                    Style::Narrow => Width::Narrow,
                };
                let formatter = ListFormatter::open(&locale.icu_locale(&[])?, icu_kind, width)
                    .or_type_error("failed to initialize ListFormat")?;
                Ok(ListFormat(Rc::new(Shared {
                    locale: locale.clone(),
                    kind,
                    style,
                    formatter,
                })))
            },
        )
    }

    pub fn locale(&self) -> &LanguageTag {
        self.0.locale.tag()
    }

    pub fn kind(&self) -> Type {
        self.0.kind
    }

    pub fn style(&self) -> Style {
        self.0.style
    }

    pub fn format(&self, list: &[StringRef]) -> Result<StringRef> {
        if let [only] = list {
            return Ok(only.clone());
        }
        let literals = self.literals(list)?;
        let pieces = || pieces(&literals, list).map(|(_, piece)| piece);
        let length = pieces().fold(0usize, |sum, piece| sum.saturating_add(piece.len()));
        let mut out = StringBuilder::with_capacity(length);
        pieces().for_each(|piece| out.push_text(piece.text()));
        Ok(out.finish()?)
    }

    pub fn format_to_parts<'a>(&self, list: &'a [StringRef]) -> Result<ListParts<'a>> {
        Ok(ListParts {
            literals: self.literals(list)?,
            list,
        })
    }

    /// Of a list that stands in for `list`. By UTS #35 all elements from the second to the last but one have the same text
    /// between them, and the start of the last element is all that a pattern depends on.
    fn literals(&self, list: &[StringRef]) -> Result<Literals> {
        const FAILURE: &str = "failed to format list of strings";
        const CONTEXT_LENGTH: usize = 8;
        /// Selects the patterns that an empty element gets.
        const NEUTRAL: &[u16] = &[0x05d0];

        let [_, .., last] = list else {
            return Ok(Literals::from_elem(StringRef::empty(), list.len() + 1));
        };
        let context: SmallVec<[u16; CONTEXT_LENGTH]> =
            last.text().code_units().take(CONTEXT_LENGTH).collect();
        let last = if context.is_empty() {
            NEUTRAL
        } else {
            &context
        };
        let count = list.len().min(MAX_STAND_INS);
        let stand_ins = core::iter::repeat_n(NEUTRAL, count - 1).chain([last]);

        let formatted = self.0.formatter.format(stand_ins).or_type_error(FAILURE)?;
        let formatted = formatted.value().or_type_error(FAILURE)?;
        let text = formatted.text().or_type_error(FAILURE)?;
        let mut elements = formatted
            .fields_matching(Category::LIST, ELEMENT_FIELD)
            .or_type_error(FAILURE)?;
        let mut literals = Literals::new();
        let mut start = 0;
        while let Some(element) = elements.next().or_type_error(FAILURE)? {
            let literal = text.get(start..element.range.start);
            literals.push(new_string(literal.unwrap_or_default())?);
            start = element.range.end;
        }
        literals.push(new_string(text.get(start..).unwrap_or_default())?);
        if literals.len() != count + 1 {
            return Err(Error::Type(FAILURE));
        }
        Ok(literals)
    }
}

const MAX_STAND_INS: usize = 4;

/// What comes before each element of a list of at most `MAX_STAND_INS`, and after the last.
type Literals = SmallVec<[StringRef; MAX_STAND_INS + 1]>;

fn pieces<'a>(
    literals: &'a Literals,
    list: &'a [StringRef],
) -> impl Iterator<Item = (PartKind, &'a StringRef)> {
    let literal_before = move |index: usize| {
        let slot = match (index + 1).cmp(&list.len()) {
            Ordering::Less => index.min(MAX_STAND_INS - 2),
            Ordering::Equal => literals.len() - 2,
            Ordering::Greater => literals.len() - 1,
        };
        let literal = &literals[slot];
        (!literal.is_empty()).then_some((PartKind::Literal, literal))
    };
    (0..=list.len()).flat_map(move |index| {
        let element = list.get(index).map(|element| (PartKind::Element, element));
        literal_before(index).into_iter().chain(element)
    })
}

pub struct ListParts<'a> {
    literals: Literals,
    list: &'a [StringRef],
}

impl ListParts<'_> {
    pub fn iter(&self) -> impl Iterator<Item = (PartKind, &StringRef)> {
        pieces(&self.literals, self.list)
    }
}
