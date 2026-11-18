use std::time::{SystemTime, UNIX_EPOCH};

use bun_engine_date::TimeZone;
use bun_engine_text::Text;

use crate::format::IsoText;
use crate::rounding::Rounding;
use crate::time_duration::split_days;
use crate::time_zone::TimeZoneRules;
use crate::unit::{NANOSECONDS_PER_DAY, NANOSECONDS_PER_SECOND, UnitGroup};
use crate::{
    DifferenceOperation, DifferenceOptions, Duration, Error, InputError, IsoDate, IsoDateTime,
    NumberError, PlainTime, Precision, PrecisionOptions, Result, RoundOptions, TextError,
    TimeDuration, Unit, parse,
};

const MILLISECONDS_PER_DAY: i64 = NANOSECONDS_PER_DAY / 1_000_000;
const SECONDS_PER_DAY: i64 = NANOSECONDS_PER_DAY / NANOSECONDS_PER_SECOND;
const DAY_LIMIT: i64 = 100_000_000;
const LIMIT: i128 = DAY_LIMIT as i128 * NANOSECONDS_PER_DAY as i128;

/// An exact time: nanoseconds since the epoch, at most 10^8 days from it. It is aligned as a word is, like a cell.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(C, packed(8))]
pub struct Instant(i128);

impl Instant {
    /// IsValidEpochNanoseconds
    #[inline]
    pub fn new(epoch_nanoseconds: i128) -> Option<Instant> {
        (-LIMIT..=LIMIT)
            .contains(&epoch_nanoseconds)
            .then_some(Instant(epoch_nanoseconds))
    }

    #[inline]
    pub(crate) fn clamped(epoch_nanoseconds: i128) -> Instant {
        Instant(epoch_nanoseconds.clamp(-LIMIT, LIMIT))
    }

    #[inline]
    pub(crate) fn from_epoch_seconds(seconds: i64) -> Option<Instant> {
        Instant::new(i128::from(seconds) * i128::from(NANOSECONDS_PER_SECOND))
    }

    /// TimeClip
    #[inline]
    fn from_clipped_milliseconds(milliseconds: f64) -> Option<Instant> {
        (milliseconds.abs() <= (DAY_LIMIT * MILLISECONDS_PER_DAY) as f64)
            .then(|| Instant(i128::from(milliseconds as i64) * 1_000_000))
    }

    pub fn from_epoch_milliseconds(milliseconds: f64) -> Result<Instant, InputError<'static>> {
        if milliseconds.fract() != 0.0 {
            return Err(InputError::Number(
                NumberError::EpochMillisecondsNotInteger,
                milliseconds,
            ));
        }
        Instant::from_clipped_milliseconds(milliseconds).ok_or(InputError::Number(
            NumberError::EpochMillisecondsOutOfRange,
            milliseconds,
        ))
    }

    /// The current time, when the embedder has set it in milliseconds.
    pub fn from_overridden_clock(milliseconds: f64) -> Result<Instant, InputError<'static>> {
        Instant::from_clipped_milliseconds(milliseconds).ok_or(InputError::Number(
            NumberError::OverriddenClock,
            milliseconds,
        ))
    }

    /// SystemUTCEpochNanoseconds
    pub fn now() -> Instant {
        Instant::clamped(match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => since.as_nanos() as i128,
            Err(before) => -(before.duration().as_nanos() as i128),
        })
    }

    pub fn parse(text: Text<'_>) -> Result<Instant, InputError<'_>> {
        parse::instant(text)
            .and_then(|(date_time, offset)| {
                Instant::new(date_time.utc_epoch_nanoseconds() - i128::from(offset))
            })
            .ok_or(InputError::Text(TextError::Instant, text))
    }

    #[inline]
    pub const fn epoch_nanoseconds(self) -> i128 {
        self.0
    }

    /// Rounded down.
    #[inline]
    pub fn epoch_milliseconds(self) -> i64 {
        let (days, rest) = split_days(self.0);
        days * MILLISECONDS_PER_DAY + rest / 1_000_000
    }

    /// Rounded down.
    #[inline]
    pub(crate) fn epoch_seconds(self) -> i64 {
        let (days, rest) = split_days(self.0);
        days * SECONDS_PER_DAY + rest / NANOSECONDS_PER_SECOND
    }

    /// What a clock shows that is ahead of UTC by less than a day.
    #[inline]
    pub(crate) fn at_offset(self, offset_seconds: i32) -> IsoDateTime {
        let offset = i64::from(offset_seconds) * NANOSECONDS_PER_SECOND;
        let (days, rest) = split_days(self.0 + i128::from(offset));
        IsoDateTime {
            date: IsoDate::from_epoch_days(days as i32),
            time: PlainTime::from_nanosecond_of_day(rest),
        }
    }

    /// GetISODateTimeFor
    #[inline]
    pub fn in_time_zone(self, time_zone: TimeZone) -> IsoDateTime {
        self.at_offset(time_zone.offset_seconds_at(self))
    }

    pub fn add(self, duration: &Duration) -> Result<Instant> {
        self.checked_add(time_units_only(duration)?)
            .ok_or(Error::InstantSumOutOfRange)
    }

    pub fn subtract(self, duration: &Duration) -> Result<Instant> {
        self.checked_add(-time_units_only(duration)?)
            .ok_or(Error::InstantRemainderOutOfRange)
    }

    /// AddInstant
    #[inline]
    pub(crate) fn checked_add(self, duration: TimeDuration) -> Option<Instant> {
        Instant::new(self.0 + duration.nanoseconds())
    }

    /// TimeDurationFromEpochNanosecondsDifference
    #[inline]
    pub(crate) fn until(self, other: Instant) -> TimeDuration {
        TimeDuration::from_valid(other.0 - self.0)
    }

    /// DifferenceTemporalInstant
    pub fn difference(
        self,
        operation: DifferenceOperation,
        other: Instant,
        options: DifferenceOptions,
    ) -> Result<Duration> {
        let settings =
            options.resolve(operation, UnitGroup::Time, Unit::Nanosecond, Unit::Second)?;
        let time = self.until(other).round(settings.rounding)?;
        Duration::from_difference(operation, time.into(), settings.largest_unit)
    }

    pub fn round(self, options: RoundOptions) -> Result<Instant> {
        let rounding = options.resolve(UnitGroup::Time)?.within_day()?;
        Ok(self.rounded(rounding))
    }

    /// RoundTemporalInstant. The increment divides a day, so the result is within the limits.
    pub(crate) fn rounded(self, rounding: Rounding) -> Instant {
        if rounding.is_to_one(Unit::Nanosecond) {
            return self;
        }
        let step = rounding.step_nanoseconds() as i64;
        let (days, rest) = split_days(self.0);
        let remainder = rest % step;
        if remainder == 0 {
            return self;
        }
        let lesser_is_odd = || ((days & (NANOSECONDS_PER_DAY / step)) ^ (rest / step)) & 1 == 1;
        let position = (2 * remainder).cmp(&step);
        let mut adjustment = -remainder;
        if rounding.mode.rounds_up(false, position, lesser_is_odd) {
            adjustment += step;
        }
        Instant(self.0 + i128::from(adjustment))
    }

    pub fn precision(options: PrecisionOptions) -> Result<Precision> {
        options.resolve(Unit::Minute, Error::HourInInstantText)
    }

    /// TemporalInstantToString
    pub fn to_text(self, time_zone: Option<TimeZone>, precision: Precision) -> IsoText {
        let rounded = self.rounded(precision.rounding);
        let offset = time_zone.map(|zone| zone.offset_seconds_at(rounded));
        let mut text = IsoText::new();
        text.push_date_time(rounded.at_offset(offset.unwrap_or(0)), precision.format);
        match offset {
            Some(seconds) => text.push_rounded_offset(seconds),
            None => text.push(b'Z'),
        }
        text
    }
}

fn time_units_only(duration: &Duration) -> Result<TimeDuration> {
    match duration.largest_unit() {
        Unit::Year => Err(Error::AddingYearsToInstant),
        Unit::Month => Err(Error::AddingMonthsToInstant),
        Unit::Week => Err(Error::AddingWeeksToInstant),
        Unit::Day => Err(Error::AddingDaysToInstant),
        _ => Ok(duration.time()),
    }
}
