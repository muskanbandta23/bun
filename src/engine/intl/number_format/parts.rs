use core::cmp::Reverse;
use core::ops::Range;

use bun_icu::formatted_value::{Category, Field, FormattedValue};
use bun_icu::number_format::field;
use smallvec::SmallVec;

use crate::error::IcuResult;
use crate::{Part, PartKind, Parts, Result};

pub(crate) type Fields = SmallVec<[Field; 8]>;

/// What the kind of a field depends on.
#[derive(Clone, Copy, Default)]
pub(crate) struct FieldContext {
    pub(crate) is_negative: bool,
    pub(crate) is_infinite: bool,
    pub(crate) is_nan: bool,
    pub(crate) is_unit_style: bool,
}

impl FieldContext {
    pub(crate) fn kind_of(self, field: Option<&Field>) -> PartKind {
        let Some(field) = field.filter(|it| it.category == Category::NUMBER) else {
            return PartKind::Literal;
        };
        match field.field {
            field::INTEGER if self.is_nan => PartKind::Nan,
            field::INTEGER if self.is_infinite => PartKind::Infinity,
            field::INTEGER => PartKind::Integer,
            field::FRACTION => PartKind::Fraction,
            field::DECIMAL_SEPARATOR => PartKind::Decimal,
            field::EXPONENT_SYMBOL => PartKind::ExponentSeparator,
            field::EXPONENT_SIGN => PartKind::ExponentMinusSign,
            field::EXPONENT => PartKind::ExponentInteger,
            field::GROUPING_SEPARATOR => PartKind::Group,
            field::CURRENCY => PartKind::Currency,
            field::PERCENT if self.is_unit_style => PartKind::Unit,
            field::PERCENT => PartKind::PercentSign,
            field::SIGN if self.is_negative => PartKind::MinusSign,
            field::SIGN => PartKind::PlusSign,
            field::MEASURE_UNIT => PartKind::Unit,
            field::COMPACT => PartKind::Compact,
            field::APPROXIMATELY_SIGN => PartKind::ApproximatelySign,
            _ => PartKind::Unknown,
        }
    }
}

pub(crate) fn collect_fields(value: FormattedValue<'_>, failure: &'static str) -> Result<Fields> {
    let mut source = value.fields().or_type_error(failure)?;
    let mut fields = Fields::new();
    while let Some(field) = source.next().or_type_error(failure)? {
        fields.try_reserve(1)?;
        fields.push(field);
    }
    Ok(fields)
}

/// Cuts `text` wherever a field starts or ends. `describe` takes the innermost field of a piece and where the piece starts.
pub(crate) fn push_flattened(
    parts: &mut Parts,
    text: &[u16],
    mut fields: Fields,
    describe: impl Fn(Option<&Field>, usize) -> Part,
) -> Result<()> {
    fields.sort_unstable_by_key(|it| {
        let Range { start, end } = it.range;
        (
            start,
            Reverse(end),
            it.category == Category::NUMBER,
            it.field,
        )
    });
    let mut done = 0;
    let mut push_until = |end: usize, field: Option<&Field>| {
        let Some(piece) = text.get(done..end) else {
            return Ok(());
        };
        let part = describe(field, done);
        done = end;
        parts.push(part, piece)
    };
    let mut open = SmallVec::<[&Field; 4]>::new();
    for field in &fields {
        while let Some(&closed) = open.last()
            && closed.range.end <= field.range.start
        {
            push_until(closed.range.end, Some(closed))?;
            open.pop();
        }
        push_until(field.range.start, open.last().copied())?;
        open.push(field);
    }
    while let Some(closed) = open.pop() {
        push_until(closed.range.end, Some(closed))?;
    }
    push_until(text.len(), None)
}

/// `describe` completes a part.
pub(crate) fn push_number_parts(
    parts: &mut Parts,
    number: FormattedValue<'_>,
    context: FieldContext,
    describe: impl Fn(PartKind) -> Part,
) -> Result<()> {
    let failure = "Failed to format a number.";
    let text = number.text().or_type_error(failure)?;
    let fields = collect_fields(number, failure)?;
    push_flattened(parts, text, fields, |field, _| {
        describe(context.kind_of(field))
    })
}
