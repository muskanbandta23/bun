use core::fmt;
use core::ops::Deref;

use bun_engine_date::TimeZone;
use bun_engine_number::NumberText;
use bun_engine_text::Text;

use crate::settings::SecondsFormat;
use crate::unit::NANOSECONDS_PER_SECOND;
use crate::{CalendarId, Duration, IsoDate, IsoDateTime, PlainTime, ShowCalendar, Unit};

/// More than the longest duration, or a date and time with all of its annotations.
const CAPACITY: usize = 127;
/// What is left for the name of a time zone, which is twice the longest that there is.
const ZONE_NAME_CAPACITY: usize = 62;

/// The ASCII text of a value.
#[derive(Clone, Copy)]
pub struct IsoText {
    bytes: [u8; CAPACITY],
    length: u8,
}

#[inline(always)]
const fn two_digits(value: u32) -> [u8; 2] {
    [b'0' + (value / 10) as u8, b'0' + (value % 10) as u8]
}

impl ShowCalendar {
    /// Whether a month of a year, or a day of a month, is written with all of the date that it holds.
    pub(crate) fn needs_full_date(self, calendar: CalendarId) -> bool {
        matches!(self, ShowCalendar::Always | ShowCalendar::Critical)
            || calendar != CalendarId::Iso8601
    }
}

impl IsoText {
    #[inline]
    pub(crate) const fn new() -> IsoText {
        IsoText {
            bytes: [0; CAPACITY],
            length: 0,
        }
    }

    #[inline]
    pub fn text(&self) -> Text<'_> {
        Text::Latin1(self)
    }

    #[inline]
    pub(crate) fn push(&mut self, character: u8) {
        self.push_slice(&[character]);
    }

    #[inline(always)]
    fn push_slice(&mut self, ascii: &[u8]) {
        let start = usize::from(self.length);
        self.bytes[start..start + ascii.len()].copy_from_slice(ascii);
        self.length += ascii.len() as u8;
    }

    #[inline(always)]
    fn push_field(&mut self, separator: u8, value: u32) {
        let [tens, ones] = two_digits(value);
        self.push_slice(&[separator, tens, ones]);
    }

    /// PadISOYear
    #[inline]
    fn push_year(&mut self, year: i32) {
        let magnitude = year.unsigned_abs();
        if !(0..=9999).contains(&year) {
            self.push_field(if year < 0 { b'-' } else { b'+' }, magnitude / 10_000 % 100);
        }
        let ([a, b], [c, d]) = (
            two_digits(magnitude / 100 % 100),
            two_digits(magnitude % 100),
        );
        self.push_slice(&[a, b, c, d]);
    }

    #[inline]
    pub(crate) fn push_year_month(&mut self, date: IsoDate) {
        self.push_year(date.year());
        self.push_field(b'-', date.month().into());
    }

    #[inline]
    pub(crate) fn push_month_day(&mut self, date: IsoDate) {
        self.push_slice(&two_digits(date.month().into()));
        self.push_field(b'-', date.day().into());
    }

    #[inline]
    pub(crate) fn push_date(&mut self, date: IsoDate) {
        self.push_year_month(date);
        self.push_field(b'-', date.day().into());
    }

    /// FormatTimeString
    #[inline]
    pub(crate) fn push_time(&mut self, time: PlainTime, format: SecondsFormat) {
        let seconds = time.second_of_day();
        self.push_slice(&two_digits(seconds / 3600));
        self.push_field(b':', seconds / 60 % 60);
        if format != SecondsFormat::Omitted {
            self.push_field(b':', seconds % 60);
            self.push_fraction(time.nanosecond_of_second(), format);
        }
    }

    /// FormatFractionalSeconds
    #[inline]
    fn push_fraction(&mut self, nanoseconds: u32, format: SecondsFormat) {
        if format == SecondsFormat::Digits(0)
            || (format == SecondsFormat::Shortest && nanoseconds == 0)
        {
            return;
        }
        let mut digits = [b'.'; 10];
        digits[1] = b'0' + (nanoseconds / 100_000_000) as u8;
        let (upper, lower) = (nanoseconds / 10_000 % 10_000, nanoseconds % 10_000);
        digits[2..4].copy_from_slice(&two_digits(upper / 100));
        digits[4..6].copy_from_slice(&two_digits(upper % 100));
        digits[6..8].copy_from_slice(&two_digits(lower / 100));
        digits[8..].copy_from_slice(&two_digits(lower % 100));
        let unused = match format {
            SecondsFormat::Digits(count) => 9 - count,
            _ => digits.iter().rev().take_while(|&&it| it == b'0').count() as u8,
        };
        self.push_slice(&digits);
        self.length -= unused;
    }

    #[inline]
    pub(crate) fn push_date_time(&mut self, date_time: IsoDateTime, format: SecondsFormat) {
        self.push_date(date_time.date);
        self.push(b'T');
        self.push_time(date_time.time, format);
    }

    /// FormatUTCOffsetNanoseconds
    pub(crate) fn push_offset(&mut self, seconds: i32) {
        let magnitude = seconds.unsigned_abs();
        self.push_field(if seconds < 0 { b'-' } else { b'+' }, magnitude / 3600);
        self.push_field(b':', magnitude / 60 % 60);
        if magnitude % 60 != 0 {
            self.push_field(b':', magnitude % 60);
        }
    }

    /// FormatDateTimeUTCOffsetRounded
    pub(crate) fn push_rounded_offset(&mut self, seconds: i32) {
        self.push_offset(seconds.signum() * ((seconds.abs() + 30) / 60 * 60));
    }

    #[inline]
    fn open_annotation(&mut self, is_critical: bool) {
        self.push_slice(b"[!");
        self.length -= u8::from(!is_critical);
    }

    pub(crate) fn push_time_zone(&mut self, time_zone: TimeZone, is_critical: bool) {
        let identifier = time_zone.identifier();
        let name = identifier.as_bytes();
        self.open_annotation(is_critical);
        self.push_slice(&name[..name.len().min(ZONE_NAME_CAPACITY)]);
        self.push(b']');
    }

    /// FormatCalendarAnnotation
    #[inline]
    pub(crate) fn push_calendar(&mut self, calendar: CalendarId, show: ShowCalendar) {
        if show == ShowCalendar::Never
            || (show == ShowCalendar::Auto && calendar == CalendarId::Iso8601)
        {
            return;
        }
        self.open_annotation(show == ShowCalendar::Critical);
        self.push_slice(b"u-ca=");
        self.push_slice(calendar.identifier().as_bytes());
        self.push(b']');
    }

    /// TemporalDurationToString
    pub(crate) fn push_duration(&mut self, duration: &Duration, format: SecondsFormat) {
        const SECOND: u64 = NANOSECONDS_PER_SECOND as u64;
        if duration.sign() < 0 {
            self.push(b'-');
        }
        self.push(b'P');
        self.push_count(duration.get(Unit::Year), b'Y');
        self.push_count(duration.get(Unit::Month), b'M');
        self.push_count(duration.get(Unit::Week), b'W');
        self.push_count(duration.get(Unit::Day), b'D');

        let nanoseconds = duration.nanoseconds_from(Unit::Second).unsigned_abs();
        let has_seconds = nanoseconds != 0
            || format != SecondsFormat::Shortest
            || duration.largest_unit() <= Unit::Second;
        let (hours, minutes) = (duration.get(Unit::Hour), duration.get(Unit::Minute));
        if hours == 0.0 && minutes == 0.0 && !has_seconds {
            return;
        }
        self.push(b'T');
        self.push_count(hours, b'H');
        self.push_count(minutes, b'M');
        if has_seconds {
            let (seconds, fraction) = match u64::try_from(nanoseconds) {
                Ok(nanoseconds) => (nanoseconds / SECOND, nanoseconds % SECOND),
                Err(_) => (
                    (nanoseconds / u128::from(SECOND)) as u64,
                    (nanoseconds % u128::from(SECOND)) as u64,
                ),
            };
            self.push_slice(&NumberText::from(seconds));
            self.push_fraction(fraction as u32, format);
            self.push(b'S');
        }
    }

    #[inline]
    fn push_count(&mut self, count: f64, designator: u8) {
        if count != 0.0 {
            self.push_slice(&NumberText::from(count.abs() as u64));
            self.push(designator);
        }
    }
}

impl Deref for IsoText {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        &self.bytes[..usize::from(self.length)]
    }
}

impl fmt::Debug for IsoText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.text().fmt(formatter)
    }
}
