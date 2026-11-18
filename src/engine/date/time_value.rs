//! Time values and the arithmetic of ECMA-262, 21.4.1.

use core::ops::RangeInclusive;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::civil::days_from_civil;

pub const MS_PER_SECOND: f64 = 1000.0;
pub const MS_PER_MINUTE: f64 = 60_000.0;
pub const MS_PER_HOUR: f64 = 3_600_000.0;
pub const MS_PER_DAY: f64 = 86_400_000.0;
pub const MAX_TIME_VALUE: f64 = 8.64e15;
pub(crate) const TIME_VALUES: RangeInclusive<i64> =
    -(MAX_TIME_VALUE as i64)..=MAX_TIME_VALUE as i64;

/// The time value of now.
pub fn current_time() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(since) => (since.as_secs() * 1000 + u64::from(since.subsec_millis())) as f64,
        Err(before) => -(before.duration().as_nanos().div_ceil(1_000_000) as f64),
    }
}

#[inline]
pub fn time_clip(time: f64) -> f64 {
    if time.abs() <= MAX_TIME_VALUE {
        time.trunc() + 0.0
    } else {
        f64::NAN
    }
}

#[inline]
pub(crate) fn time_clip_integer(time: i64) -> f64 {
    if TIME_VALUES.contains(&time) {
        time as f64
    } else {
        f64::NAN
    }
}

/// The result is not finite if an argument is not, which `make_date` turns into NaN.
#[inline]
pub fn make_time(hour: f64, minute: f64, second: f64, millisecond: f64) -> f64 {
    hour.trunc() * MS_PER_HOUR
        + minute.trunc() * MS_PER_MINUTE
        + second.trunc() * MS_PER_SECOND
        + millisecond.trunc()
}

/// `month` is from zero. NaN if the year that it falls in does not fit in 32 bits.
#[inline]
pub fn make_day(year: f64, month: f64, date: f64) -> f64 {
    let month = month.trunc();
    let years_in_months = (month / 12.0).floor();
    let year = year.trunc() + years_in_months;
    let month = month - years_in_months * 12.0;
    if f64::from(year as i32) != year || !(0.0..12.0).contains(&month) {
        return f64::NAN;
    }
    days_from_civil(year as i32, month as u8 + 1, 1) as f64 + date.trunc() - 1.0
}

#[inline]
pub fn make_date(day: f64, time: f64) -> f64 {
    let date = day * MS_PER_DAY + time;
    if date.is_finite() { date } else { f64::NAN }
}

#[inline]
pub fn make_full_year(year: f64) -> f64 {
    let year = year.trunc();
    if (0.0..=99.0).contains(&year) {
        1900.0 + year
    } else {
        year
    }
}

/// `make_date` of `make_day` and `make_time`.
#[inline]
pub fn make_date_from_integers(components: [i32; 7]) -> f64 {
    let [year, month, day, hour, minute, second, millisecond] = components;
    let Some(year) = year.checked_add(month.div_euclid(12)) else {
        return f64::NAN;
    };
    let days = days_from_civil(year, month.rem_euclid(12) as u8 + 1, 0) + i64::from(day);
    let time = i64::from(hour) * 3_600_000
        + i64::from(minute) * 60_000
        + i64::from(second) * 1000
        + i64::from(millisecond);
    days as f64 * MS_PER_DAY + time as f64
}

/// msFromTime of a time value that is not NaN.
#[inline]
pub fn ms_from_time(time: f64) -> u16 {
    (time as i64).rem_euclid(1000) as u16
}
