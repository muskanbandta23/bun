use bun_engine_text::Text;

use crate::format::IsoText;
use crate::unit::NANOSECONDS_PER_SECOND;
use crate::{
    DateDuration, DifferenceOperation, Error, InputError, InternalDuration, Precision,
    PrecisionOptions, Result, TextError, TimeDuration, Unit, parse,
};

const UNIT_COUNT: usize = Unit::ALL.len();

/// What a `Temporal.Duration` holds: an integer for each unit, for which IsValidDuration is true. None is -0.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Duration([f64; UNIT_COUNT]);

/// A Partial Duration Record. A field that is not set is NaN.
#[derive(Clone, Copy, Debug)]
pub struct PartialDuration([f64; UNIT_COUNT]);

impl Default for PartialDuration {
    fn default() -> PartialDuration {
        PartialDuration([f64::NAN; UNIT_COUNT])
    }
}

impl PartialDuration {
    /// The validation of ToIntegerIfIntegral.
    #[inline]
    pub fn set(&mut self, unit: Unit, number: f64) -> Result<()> {
        if number.fract() != 0.0 {
            return Err(Error::DurationFieldNotInteger);
        }
        self.0[unit as usize] = number + 0.0;
        Ok(())
    }
}

impl Eq for Duration {}

impl Duration {
    pub const ZERO: Duration = Duration([0.0; UNIT_COUNT]);

    /// The constructor.
    pub fn new(arguments: &PartialDuration) -> Result<Duration> {
        Duration::ZERO.merged(arguments)
    }

    /// ToTemporalDuration of an object.
    pub fn from_partial(partial: &PartialDuration) -> Result<Duration> {
        Duration::ZERO.with(partial)
    }

    pub fn with(&self, partial: &PartialDuration) -> Result<Duration> {
        if partial.0.iter().all(|field| field.is_nan()) {
            return Err(Error::NoDurationFields);
        }
        self.merged(partial)
    }

    fn merged(&self, partial: &PartialDuration) -> Result<Duration> {
        let mut fields = self.0;
        for (field, &given) in fields.iter_mut().zip(&partial.0) {
            *field = if given.is_nan() { *field } else { given };
        }
        Duration::from_integers(&fields).ok_or(Error::InvalidDuration)
    }

    pub fn parse(text: Text<'_>) -> Result<Duration, InputError<'_>> {
        let fields = parse::duration(text).ok_or(InputError::Text(TextError::Duration, text))?;
        Ok(Duration::from_integers(&fields).ok_or(Error::InvalidDuration)?)
    }

    /// IsValidDuration
    fn from_integers(fields: &[f64; UNIT_COUNT]) -> Option<Duration> {
        const CALENDAR_LIMIT: f64 = (1u64 << 32) as f64;
        /// Within the limit of 2^53 seconds by more than any rounding.
        const SURELY_VALID: f64 = 9.0e24;
        const SURELY_INVALID: f64 = 9.1e24;

        let has_negative = fields.iter().fold(false, |any, &it| any | (it < 0.0));
        let has_positive = fields.iter().fold(false, |any, &it| any | (it > 0.0));
        let (calendar_fields, time_fields) = fields.split_at(Unit::Day as usize);
        if (has_negative && has_positive)
            || calendar_fields.iter().any(|it| it.abs() >= CALENDAR_LIMIT)
        {
            return None;
        }
        let estimate = (time_fields.iter().zip(&Unit::ALL[Unit::Day as usize..]))
            .fold(0.0, |sum, (&field, unit)| {
                sum + field * unit.nanoseconds() as f64
            })
            .abs();
        let duration = Duration(*fields);
        let is_valid = estimate < SURELY_VALID
            || (estimate < SURELY_INVALID
                && TimeDuration::new(duration.nanoseconds_from(Unit::Day)).is_some());
        is_valid.then_some(duration)
    }

    /// TemporalDurationFromInternal
    pub(crate) fn from_internal(
        internal: InternalDuration,
        largest_unit: Unit,
    ) -> Result<Duration> {
        const SECOND: u64 = NANOSECONDS_PER_SECOND as u64;
        let magnitude = internal.time.nanoseconds().unsigned_abs();
        let (seconds, fraction) = match u64::try_from(magnitude) {
            Ok(magnitude) => (magnitude / SECOND, magnitude % SECOND),
            Err(_) => (
                (magnitude / u128::from(SECOND)) as u64,
                (magnitude % u128::from(SECOND)) as u64,
            ),
        };
        let (minutes, hours) = (seconds / 60, seconds / 3600);
        let [days, hours, minutes, seconds_field] = match largest_unit {
            Unit::Year | Unit::Month | Unit::Week | Unit::Day => {
                [hours / 24, hours % 24, minutes % 60, seconds % 60]
            }
            Unit::Hour => [0, hours, minutes % 60, seconds % 60],
            Unit::Minute => [0, 0, minutes, seconds % 60],
            Unit::Second => [0, 0, 0, seconds],
            Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond => [0; 4],
        };
        let mut subseconds = [
            fraction / 1_000_000,
            fraction / 1000 % 1000,
            fraction % 1000,
        ]
        .map(|it| it as f64);
        match largest_unit {
            Unit::Millisecond => subseconds[0] = (seconds * 1000 + fraction / 1_000_000) as f64,
            Unit::Microsecond => {
                let whole = u128::from(seconds) * 1_000_000 + u128::from(fraction / 1000);
                subseconds[..2].copy_from_slice(&[0.0, whole as f64]);
            }
            Unit::Nanosecond => subseconds = [0.0, 0.0, magnitude as f64],
            _ => {}
        }

        let is_negative = internal.time.sign() < 0;
        let signed = |magnitude: f64| {
            if is_negative {
                0.0 - magnitude
            } else {
                magnitude
            }
        };
        let date = internal.date;
        Duration::from_integers(&[
            date.years as f64,
            date.months as f64,
            date.weeks as f64,
            date.days as f64 + signed(days as f64),
            signed(hours as f64),
            signed(minutes as f64),
            signed(seconds_field as f64),
            signed(subseconds[0]),
            signed(subseconds[1]),
            signed(subseconds[2]),
        ])
        .ok_or(Error::DurationOutOfRange)
    }

    /// The last steps of `until` and `since`.
    #[inline]
    pub(crate) fn from_difference(
        operation: DifferenceOperation,
        internal: InternalDuration,
        largest_unit: Unit,
    ) -> Result<Duration> {
        let duration = Duration::from_internal(internal, largest_unit)?;
        Ok(match operation {
            DifferenceOperation::Until => duration,
            DifferenceOperation::Since => duration.negated(),
        })
    }

    #[inline]
    pub fn get(&self, unit: Unit) -> f64 {
        self.0[unit as usize]
    }

    /// DurationSign
    #[inline]
    pub fn sign(&self) -> i8 {
        let largest = self.get(self.largest_unit());
        i8::from(largest > 0.0) - i8::from(largest < 0.0)
    }

    #[inline]
    pub fn negated(&self) -> Duration {
        Duration(self.0.map(|field| 0.0 - field))
    }

    #[inline]
    pub fn abs(&self) -> Duration {
        Duration(self.0.map(f64::abs))
    }

    /// DefaultTemporalLargestUnit
    #[inline]
    pub(crate) fn largest_unit(&self) -> Unit {
        Unit::ALL
            .into_iter()
            .find(|&unit| self.get(unit) != 0.0)
            .unwrap_or(Unit::Nanosecond)
    }

    /// The sum of the fields from `largest_unit` down, leaving out years, months and weeks.
    #[inline]
    pub fn nanoseconds_from(&self, largest_unit: Unit) -> i128 {
        Unit::ALL[largest_unit.min(Unit::Day) as usize..]
            .iter()
            .map(|&unit| exact(self.get(unit)) * i128::from(unit.nanoseconds()))
            .sum()
    }

    #[inline]
    pub(crate) fn date(&self) -> DateDuration {
        DateDuration {
            years: self.get(Unit::Year) as i64,
            months: self.get(Unit::Month) as i64,
            weeks: self.get(Unit::Week) as i64,
            days: self.get(Unit::Day) as i64,
        }
    }

    #[inline]
    pub(crate) fn time(&self) -> TimeDuration {
        TimeDuration::from_valid(self.nanoseconds_from(Unit::Hour))
    }

    #[inline]
    pub(crate) fn time_with_24_hour_days(&self) -> TimeDuration {
        TimeDuration::from_valid(self.nanoseconds_from(Unit::Day))
    }

    /// ToInternalDurationRecord
    #[inline]
    pub fn to_internal(&self) -> InternalDuration {
        InternalDuration {
            date: self.date(),
            time: self.time(),
        }
    }

    /// ToDateDurationRecordWithoutTime
    #[inline]
    pub(crate) fn date_with_whole_days_of_time(&self) -> DateDuration {
        let date = self.date();
        date.with_days(date.days + self.time().whole_days())
    }

    /// AddDurations
    pub fn add(&self, other: &Duration) -> Result<Duration> {
        let largest_unit = self.largest_unit().max(other.largest_unit());
        if largest_unit.is_calendar_unit() {
            return Err(Error::AddingCalendarUnits);
        }
        let sum = self.nanoseconds_from(Unit::Day) + other.nanoseconds_from(Unit::Day);
        let time = TimeDuration::new(sum).ok_or(Error::DurationSumOutOfRange)?;
        Duration::from_internal(time.into(), largest_unit)
    }

    pub fn subtract(&self, other: &Duration) -> Result<Duration> {
        self.add(&other.negated())
    }

    pub fn precision(options: PrecisionOptions) -> Result<Precision> {
        options.resolve(Unit::Second, Error::SmallestUnitAboveSecond)
    }

    /// Cannot fail with `Precision::AUTO`.
    pub fn to_text(&self, precision: Precision) -> Result<IsoText> {
        let mut text = IsoText::new();
        if precision.rounding.is_to_one(Unit::Nanosecond) {
            text.push_duration(self, precision.format);
            return Ok(text);
        }
        let internal = InternalDuration {
            date: self.date(),
            time: self.time().round(precision.rounding)?,
        };
        let rounded = Duration::from_internal(internal, self.largest_unit().max(Unit::Second))?;
        text.push_duration(&rounded, precision.format);
        Ok(text)
    }
}

#[inline]
fn exact(integer: f64) -> i128 {
    if integer.abs() < (1u64 << 63) as f64 {
        i128::from(integer as i64)
    } else {
        integer as i128
    }
}
