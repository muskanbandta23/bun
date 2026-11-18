use bun_engine_text::{PropertyName, Text, names};

use crate::format::IsoText;
use crate::{
    DateDuration, DifferenceOperation, Error, InputError, InternalDuration, Precision,
    PrecisionOptions, Result, TextError, TimeDuration, Unit, parse,
};

const UNIT_COUNT: usize = Unit::ALL.len();
/// There are fewer years, months and weeks than this.
const CALENDAR_LIMIT: u64 = 1 << 32;

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
    /// In the order that ToTemporalPartialDurationRecord reads them.
    pub const PROPERTIES: [(Unit, PropertyName<'static>); UNIT_COUNT] = [
        (Unit::Day, names::days),
        (Unit::Hour, names::hours),
        (Unit::Microsecond, names::microseconds),
        (Unit::Millisecond, names::milliseconds),
        (Unit::Minute, names::minutes),
        (Unit::Month, names::months),
        (Unit::Nanosecond, names::nanoseconds),
        (Unit::Second, names::seconds),
        (Unit::Week, names::weeks),
        (Unit::Year, names::years),
    ];

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
        let mut merged = *self;
        for (field, &given) in merged.0.iter_mut().zip(&partial.0) {
            *field = if given.is_nan() { *field } else { given };
        }
        merged.check(Error::InvalidDuration)?;
        Ok(merged)
    }

    pub fn parse(text: Text<'_>) -> Result<Duration, InputError<'_>> {
        let mut parsed = Duration::ZERO;
        parse::duration(text, &mut parsed.0).ok_or(InputError::Text(TextError::Duration, text))?;
        parsed.check(Error::InvalidDuration)?;
        Ok(parsed)
    }

    /// IsValidDuration, for fields that are integers.
    fn check(&self, invalid: Error) -> Result<()> {
        /// Further from the limit of 2^53 seconds than any rounding error.
        const SURELY_VALID: f64 = 9.0e24;
        const SURELY_INVALID: f64 = 9.1e24;

        let has_negative = self.0.iter().fold(false, |any, &it| any | (it < 0.0));
        let has_positive = self.0.iter().fold(false, |any, &it| any | (it > 0.0));
        let estimate = self.estimate_nanoseconds_from(Unit::Day).abs();
        let is_valid = !(has_negative && has_positive)
            && (self.0[..Unit::Day as usize].iter()).all(|it| it.abs() < CALENDAR_LIMIT as f64)
            && (estimate < SURELY_VALID
                || (estimate < SURELY_INVALID
                    && TimeDuration::new(self.nanoseconds_from(Unit::Day)).is_some()));
        if is_valid { Ok(()) } else { Err(invalid) }
    }

    /// TemporalDurationFromInternal
    #[inline]
    pub(crate) fn from_internal(
        internal: InternalDuration,
        largest_unit: Unit,
    ) -> Result<Duration> {
        let InternalDuration { date, time } = internal;
        let date_fields = [date.years, date.months, date.weeks, date.days];
        let mut duration = Duration::ZERO;
        if time != TimeDuration::ZERO {
            balance(time, largest_unit, &mut duration.0);
        }
        for (field, count) in duration.0.iter_mut().zip(date_fields) {
            *field += count as f64;
        }
        let is_surely_valid = i64::try_from(time.nanoseconds()).is_ok()
            && (date_fields.iter()).all(|it| it.unsigned_abs() < CALENDAR_LIMIT)
            && date.sign() * time.sign() >= 0;
        if !is_surely_valid {
            duration.check(Error::DurationOutOfRange)?;
        }
        Ok(duration)
    }

    /// The last steps of `until` and `since`.
    #[inline]
    pub(crate) fn from_difference(
        operation: DifferenceOperation,
        internal: InternalDuration,
        largest_unit: Unit,
    ) -> Result<Duration> {
        let internal = match operation {
            DifferenceOperation::Until => internal,
            DifferenceOperation::Since => -internal,
        };
        Duration::from_internal(internal, largest_unit)
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
        let estimate = self.estimate_nanoseconds_from(largest_unit);
        if estimate.abs() < (1u64 << f64::MANTISSA_DIGITS) as f64 {
            return i128::from(estimate as i64);
        }
        Unit::ALL[largest_unit.min(Unit::Day) as usize..]
            .iter()
            .map(|&unit| exact(self.get(unit)) * i128::from(unit.nanoseconds()))
            .sum()
    }

    /// Exact below 2^53, because the terms are integers of one sign.
    #[inline(always)]
    fn estimate_nanoseconds_from(&self, largest_unit: Unit) -> f64 {
        let term = |unit: Unit| {
            if unit <= largest_unit {
                self.get(unit) * unit.nanoseconds() as f64
            } else {
                0.0
            }
        };
        ((term(Unit::Day) + term(Unit::Hour)) + (term(Unit::Minute) + term(Unit::Second)))
            + ((term(Unit::Millisecond) + term(Unit::Microsecond)) + term(Unit::Nanosecond))
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

/// Writes the fields from `largest_unit`, or from the days, down.
fn balance(time: TimeDuration, largest_unit: Unit, fields: &mut [f64; UNIT_COUNT]) {
    let (seconds, fraction) = time.magnitude_in_seconds();
    let (minutes, hours) = (seconds / 60, seconds / 3600);
    let whole = match largest_unit {
        Unit::Year | Unit::Month | Unit::Week | Unit::Day => {
            [hours / 24, hours % 24, minutes % 60, seconds % 60]
        }
        Unit::Hour => [0, hours, minutes % 60, seconds % 60],
        Unit::Minute => [0, 0, minutes, seconds % 60],
        Unit::Second => [0, 0, 0, seconds],
        Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond => [0; 4],
    };
    let parts = [
        fraction / 1_000_000,
        fraction / 1000 % 1000,
        fraction % 1000,
    ]
    .map(u64::from);
    let sign = i64::from(time.sign());
    let counts = whole.into_iter().chain(parts);
    for (field, count) in fields[Unit::Day as usize..].iter_mut().zip(counts) {
        *field = (count as i64 * sign) as f64;
    }
    if largest_unit < Unit::Second {
        fields[Unit::Millisecond as usize..largest_unit as usize].fill(0.0);
        fields[largest_unit as usize] =
            (time.nanoseconds() / i128::from(largest_unit.nanoseconds())) as f64;
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
