use bun_engine_date::days_in_month;
use bun_engine_text::{CodeUnit, Text, with_text};

use super::cursor::Cursor;
use super::{Annotations, Offset};
use crate::calendar::ISO_REFERENCE_YEAR;
use crate::{IsoDate, IsoDateTime, PlainTime};

/// An ISO Date-Time Parse Record with a date.
#[derive(Clone, Copy)]
pub(crate) struct DateTime<'a> {
    pub(crate) date: IsoDate,
    pub(crate) time: Option<PlainTime>,
    pub(crate) offset: Offset,
    pub(crate) annotations: Annotations<'a>,
}

fn date_time(text: Text<'_>) -> Option<DateTime<'_>> {
    with_text!(text, |units| Cursor::parse_all(
        units,
        Cursor::annotated_date_time
    ))
}

fn date_time_in_calendar(text: Text<'_>) -> Option<DateTime<'_>> {
    date_time(text).filter(|parsed| parsed.annotations.can_name_calendar())
}

/// TemporalDateTimeString[~Zoned]
pub(crate) fn plain_date_time(text: Text<'_>) -> Option<DateTime<'_>> {
    date_time_in_calendar(text).filter(|parsed| parsed.offset != Offset::Utc)
}

/// TemporalDateTimeString[+Zoned]
pub(crate) fn zoned_date_time(text: Text<'_>) -> Option<DateTime<'_>> {
    date_time_in_calendar(text).filter(|parsed| parsed.annotations.time_zone.is_some())
}

/// Either kind of TemporalDateTimeString.
pub(crate) fn relative_to(text: Text<'_>) -> Option<DateTime<'_>> {
    date_time_in_calendar(text)
        .filter(|parsed| parsed.offset != Offset::Utc || parsed.annotations.time_zone.is_some())
}

/// TemporalInstantString, with the offset in nanoseconds.
pub(crate) fn instant(text: Text<'_>) -> Option<(IsoDateTime, i64)> {
    let parsed = date_time(text)?;
    let offset = match parsed.offset {
        Offset::Absent => return None,
        Offset::Utc => 0,
        Offset::Numeric { nanoseconds, .. } => nanoseconds,
    };
    let (date, time) = (parsed.date, parsed.time?);
    Some((IsoDateTime { date, time }, offset))
}

/// TemporalTimeString
pub(crate) fn time(text: Text<'_>) -> Option<PlainTime> {
    match annotated_time(text) {
        Some((time, ..)) => Some(time),
        None => {
            date_time(text)
                .filter(|parsed| parsed.offset != Offset::Utc)?
                .time
        }
    }
}

/// TemporalYearMonthString. The day is 1 if it is not written.
pub(crate) fn year_month(text: Text<'_>) -> Option<DateTime<'_>> {
    annotated_year_month(text).or_else(|| plain_date_time(text))
}

/// TemporalMonthDayString. The year is a leap year if it is not written.
pub(crate) fn month_day(text: Text<'_>) -> Option<DateTime<'_>> {
    annotated_month_day(text).or_else(|| plain_date_time(text))
}

/// For ParseTemporalCalendarString and ParseTemporalTimeZoneString.
pub(crate) fn any_date_or_time(text: Text<'_>) -> Option<(Offset, Annotations<'_>)> {
    date_time(text)
        .or_else(|| annotated_year_month(text))
        .or_else(|| annotated_month_day(text))
        .map(|parsed| (parsed.offset, parsed.annotations))
        .or_else(|| annotated_time(text).map(|(_, offset, annotations)| (offset, annotations)))
}

fn annotated_year_month(text: Text<'_>) -> Option<DateTime<'_>> {
    with_text!(text, |units| Cursor::parse_all(units, |cursor| {
        cursor.annotated_part_of_date(Cursor::year_month)
    }))
}

fn annotated_month_day(text: Text<'_>) -> Option<DateTime<'_>> {
    with_text!(text, |units| Cursor::parse_all(units, |cursor| {
        cursor.annotated_part_of_date(Cursor::month_day)
    }))
}

fn annotated_time(text: Text<'_>) -> Option<(PlainTime, Offset, Annotations<'_>)> {
    with_text!(text, |units| Cursor::parse_all(
        units,
        Cursor::annotated_time
    ))
}

impl<'a, T: CodeUnit> Cursor<'a, T> {
    #[inline]
    fn year(&mut self) -> Option<i32> {
        let sign = self.sign();
        let mut year = i32::from(self.two_digits()?) * 100 + i32::from(self.two_digits()?);
        if let Some(sign) = sign {
            year = (year * 100 + i32::from(self.two_digits()?)) * i32::from(sign);
            if year == 0 && sign < 0 {
                return None;
            }
        }
        Some(year)
    }

    #[inline]
    fn month(&mut self) -> Option<u8> {
        self.two_digits_up_to(12).filter(|&month| month != 0)
    }

    #[inline]
    fn day_of(&mut self, year: i32, month: u8) -> Option<IsoDate> {
        let day = self.two_digits_up_to(days_in_month(year, month))?;
        (day != 0).then(|| IsoDate::from_valid(year, month, day))
    }

    #[inline]
    fn date(&mut self) -> Option<IsoDate> {
        let year = self.year()?;
        let is_extended = self.eat(b'-');
        let month = self.month()?;
        if is_extended {
            self.expect(b'-')?;
        }
        self.day_of(year, month)
    }

    /// DateSpecYearMonth
    fn year_month(&mut self) -> Option<IsoDate> {
        let year = self.year()?;
        self.eat(b'-');
        Some(IsoDate::from_valid(year, self.month()?, 1))
    }

    /// DateSpecMonthDay
    fn month_day(&mut self) -> Option<IsoDate> {
        if self.eat(b'-') {
            self.expect(b'-')?;
        }
        let month = self.month()?;
        self.eat(b'-');
        self.day_of(ISO_REFERENCE_YEAR, month)
    }

    #[inline]
    fn annotated_date_time(&mut self) -> Option<DateTime<'a>> {
        let date = self.date()?;
        let (mut time, mut offset) = (None, Offset::Absent);
        if self.eat_if(|next| matches!(next, b'T' | b't' | b' ')) {
            time = Some(self.time()?);
            offset = self.date_time_offset()?;
        }
        Some(DateTime {
            date,
            time,
            offset,
            annotations: self.annotations()?,
        })
    }

    /// AnnotatedYearMonth or AnnotatedMonthDay, which only the ISO 8601 calendar has.
    fn annotated_part_of_date(
        &mut self,
        part: impl FnOnce(&mut Self) -> Option<IsoDate>,
    ) -> Option<DateTime<'a>> {
        Some(DateTime {
            date: part(self)?,
            time: None,
            offset: Offset::Absent,
            annotations: self.annotations().filter(Annotations::is_iso_calendar)?,
        })
    }

    fn annotated_time(&mut self) -> Option<(PlainTime, Offset, Annotations<'a>)> {
        if !self.eat_letter(b'T')
            && (self.reads_as(Cursor::month_day) || self.reads_as(Cursor::year_month))
        {
            return None;
        }
        let time = self.time()?;
        let offset = self.date_time_offset().filter(|&it| it != Offset::Utc)?;
        Some((time, offset, self.annotations()?))
    }

    /// Whether `part` accepts everything before the annotations.
    fn reads_as(&self, part: impl FnOnce(&mut Self) -> Option<IsoDate>) -> bool {
        let mut rest = *self;
        part(&mut rest).is_some() && matches!(rest.peek(), b'[' | 0)
    }
}
