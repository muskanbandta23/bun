use bun_engine_date::TimeZone;
use bun_engine_text::Text;
use bun_engine_vm::StringView;

use super::prelude::*;
use crate::{
    CalendarId, InputError, Instant, IsoText, TemporalValue, TextError, ZonedDateTime,
    parse_time_zone, to_integer_with_truncation,
};

/// ToIntegerWithTruncation
#[inline]
pub(super) fn to_integer(cx: &mut Cx, value: Value, not_finite: Error) -> JsResult<i32> {
    to_integer_with_truncation(value.to_number(cx)?, not_finite).or_throw(cx)
}

#[inline]
pub(super) fn to_optional_integer(
    cx: &mut Cx,
    value: Value,
    not_finite: Error,
) -> JsResult<Option<i32>> {
    if value.is_undefined() {
        return Ok(None);
    }
    to_integer(cx, value, not_finite).map(Some)
}

#[inline]
pub(super) fn require_string(cx: &Cx, value: Value, otherwise: Error) -> JsResult<StringView> {
    (value.as_string())
        .ok_or_else(|| throw(cx, otherwise))?
        .view(cx)
}

#[inline]
pub(super) fn require_object(cx: &Cx, value: Value, otherwise: Error) -> JsResult<Gc<JsObject>> {
    value.as_object().ok_or_else(|| throw(cx, otherwise))
}

/// ToBigInt and IsValidEpochNanoseconds
pub(super) fn to_epoch_nanoseconds(
    cx: &mut Cx,
    value: Value,
    out_of_range: TextError,
) -> JsResult<Instant> {
    let number = value.to_bigint(cx)?;
    if let Some(instant) = number.value().to_i128().and_then(Instant::new) {
        return Ok(instant);
    }
    let digits = match number.to_string_ref(cx, 10) {
        Ok(digits) => Some(digits),
        Err(thrown) => cx.catch(thrown).map(|_| None)?,
    };
    let text = (digits.as_ref()).map_or(Text::from_ascii("The given number of"), |it| it.text());
    Err(throw(cx, InputError::Text(out_of_range, text)))
}

/// That of the constructor of a plain type.
pub(super) fn calendar_argument(cx: &Cx, value: Value, not_string: Error) -> JsResult<CalendarId> {
    if value.is_undefined() {
        return Ok(CalendarId::Iso8601);
    }
    CalendarId::argument(require_string(cx, value, not_string)?.text()).or_throw(cx)
}

fn calendar_slot(object: &JsObject) -> Option<CalendarId> {
    match TemporalValue::of(object)? {
        TemporalValue::PlainDate(value) => Some(value.calendar()),
        TemporalValue::PlainDateTime(value) => Some(value.calendar()),
        TemporalValue::PlainMonthDay(value) => Some(value.calendar()),
        TemporalValue::PlainYearMonth(value) => Some(value.calendar()),
        TemporalValue::ZonedDateTime(value) => Some(value.calendar()),
        TemporalValue::Duration(_) | TemporalValue::Instant(_) | TemporalValue::PlainTime(_) => {
            None
        }
    }
}

/// ToTemporalCalendarIdentifier
pub(super) fn to_calendar(cx: &Cx, value: Value) -> JsResult<CalendarId> {
    if let Some(calendar) = value.as_object().and_then(|it| calendar_slot(&it)) {
        return Ok(calendar);
    }
    let string = require_string(cx, value, Error::NotConvertibleToCalendar)?;
    CalendarId::parse(string.text()).or_throw(cx)
}

/// GetTemporalCalendarIdentifierWithISODefault
pub(super) fn calendar_of(cx: &mut Cx, item: &JsObject) -> JsResult<CalendarId> {
    if let Some(calendar) = calendar_slot(item) {
        return Ok(calendar);
    }
    let value = item.get(cx, names::calendar)?;
    if value.is_undefined() {
        return Ok(CalendarId::Iso8601);
    }
    to_calendar(cx, value)
}

/// ToTemporalTimeZoneIdentifier
pub(super) fn to_time_zone(cx: &Cx, value: Value) -> JsResult<TimeZone> {
    if let Some(zoned) = ZonedDateTime::of_value(value) {
        return Ok(zoned.time_zone());
    }
    let string = require_string(cx, value, Error::NotConvertibleToTimeZone)?;
    parse_time_zone(string.text()).or_throw(cx)
}

#[inline]
pub(super) fn text_value(cx: &Cx, text: &IsoText) -> JsResult<Value> {
    Ok(JsString::from_text(cx, text.text()).into())
}

pub(super) fn time_zone_value(cx: &Cx, time_zone: TimeZone) -> JsResult<Value> {
    Ok(JsString::from_text(cx, Text::from_ascii(&time_zone.identifier())).into())
}
