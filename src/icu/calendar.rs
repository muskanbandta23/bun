//! Calendar systems, and the rules of the time zone that a calendar is in. Times are milliseconds since the epoch.

use core::ffi::{CStr, c_char};

use crate::argument::{locale_id, time_value, zone_id_length};
use crate::enumeration::UEnumeration;
use crate::error::Status;
use crate::ffi::{call, filled, open};
use crate::{Buffer, Enumeration, Result};

bun_opaque::opaque_ffi! { pub(crate) struct UCalendar; }

icu_functions! {
    fn ucal_open(zone: *const u16, length: i32, locale: *const c_char, kind: i32, status: &mut Status) -> *mut UCalendar;
    fn ucal_close(calendar: *mut UCalendar);
    fn ucal_clone(calendar: *const UCalendar, status: &mut Status) -> *mut UCalendar;
    fn ucal_setTimeZone(calendar: *mut UCalendar, zone: *const u16, length: i32, status: &mut Status);
    fn ucal_setGregorianChange(calendar: *mut UCalendar, time: f64, status: &mut Status);
    fn ucal_getMillis(calendar: *const UCalendar, status: &mut Status) -> f64;
    fn ucal_setMillis(calendar: *mut UCalendar, time: f64, status: &mut Status);
    fn ucal_get(calendar: *const UCalendar, field: Field, status: &mut Status) -> i32;
    fn ucal_set(calendar: *mut UCalendar, field: Field, value: i32);
    fn ucal_add(calendar: *mut UCalendar, field: Field, amount: i32, status: &mut Status);
    fn ucal_clear(calendar: *mut UCalendar);
    fn ucal_getLimit(calendar: *const UCalendar, field: Field, limit: Limit, status: &mut Status) -> i32;
    fn ucal_getAttribute(calendar: *const UCalendar, attribute: i32) -> i32;
    fn ucal_getDayOfWeekType(calendar: *const UCalendar, day: i32, status: &mut Status) -> i32;
    fn ucal_getTimeZoneTransitionDate(calendar: *const UCalendar, direction: Direction, time: &mut f64, status: &mut Status) -> i8;
    fn ucal_getTimeZoneOffsetFromLocal(
        calendar: *const UCalendar,
        skipped: Side,
        repeated: Side,
        standard: &mut i32,
        daylight: &mut i32,
        status: &mut Status,
    );
    fn ucal_getTimeZoneDisplayName(
        calendar: *const UCalendar,
        kind: ZoneName,
        locale: *const c_char,
        out: *mut u16,
        capacity: i32,
        status: &mut Status,
    ) -> i32;
    fn ucal_getKeywordValuesForLocale(key: *const c_char, locale: *const c_char, commonly_used: i8, status: &mut Status) -> *mut UEnumeration;
}

const UCAL_DEFAULT: i32 = 0;
const UCAL_FIRST_DAY_OF_WEEK: i32 = 1;
const UCAL_WEEKDAY: i32 = 0;
const UCAL_WEEKEND_ONSET: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Field {
    Era = 0,
    Year = 1,
    /// From zero.
    Month = 2,
    DayOfMonth = 5,
    DayOfYear = 6,
    HourOfDay = 11,
    ZoneOffset = 15,
    DaylightOffset = 16,
    ExtendedYear = 19,
    IsLeapMonth = 22,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Limit {
    /// The smallest of the maxima that the field has on any date.
    LeastMaximum = 3,
    /// On the date that the calendar is set to.
    ActualMaximum = 5,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Direction {
    Next = 0,
    Previous = 2,
    PreviousInclusive = 3,
}

/// Which offset applies to a local time that a transition skips or repeats.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Side {
    BeforeTransition = 0x04,
    AfterTransition = 0x0c,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum ZoneName {
    Standard = 0,
    Daylight = 2,
}

/// In milliseconds. Their sum is what local time is ahead of UTC.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ZoneOffset {
    pub standard: i32,
    pub daylight: i32,
}

icu_handle! {
    /// An instant and its fields, each computed from the other when it is read.
    pub struct Calendar(UCalendar), ucal_close
}

impl Calendar {
    /// The calendar system that `locale` names or prefers. A zone that ICU does not know has the offset zero.
    pub fn open(zone: &[u16], locale: &CStr) -> Result<Calendar> {
        let locale = locale_id(locale)?;
        let length = zone_id_length(zone)?;
        // SAFETY: `zone` has `length` units and `locale` is terminated. Both are copied.
        open(|status| unsafe { ucal_open(zone.as_ptr(), length, locale, UCAL_DEFAULT, status) })
            .map(Calendar)
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut UCalendar {
        self.0.as_ptr()
    }

    pub fn try_clone(&self) -> Result<Calendar> {
        // SAFETY: the calendar is open.
        open(|status| unsafe { ucal_clone(self.0.as_ptr(), status) }).map(Calendar)
    }

    pub fn set_time_zone(&mut self, zone: &[u16]) -> Result<()> {
        let length = zone_id_length(zone)?;
        // SAFETY: the calendar is open, and `zone`, which is copied, has `length` units.
        call(|status| unsafe { ucal_setTimeZone(self.0.as_ptr(), zone.as_ptr(), length, status) })
    }

    /// Dates before `time` are Julian. `Error::UNSUPPORTED` unless the system is `gregorian` or `iso8601`.
    pub fn set_gregorian_change(&mut self, time: f64) -> Result<()> {
        let time = time_value(time)?;
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_setGregorianChange(self.0.as_ptr(), time, status) })
    }

    pub fn millis(&mut self) -> Result<f64> {
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_getMillis(self.0.as_ptr(), status) })
    }

    pub fn set_millis(&mut self, time: f64) -> Result<()> {
        let time = time_value(time)?;
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_setMillis(self.0.as_ptr(), time, status) })
    }

    pub fn get(&mut self, field: Field) -> Result<i32> {
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_get(self.0.as_ptr(), field, status) })
    }

    pub fn set(&mut self, field: Field, value: i32) {
        // SAFETY: the calendar is open.
        unsafe { ucal_set(self.0.as_ptr(), field, value) }
    }

    pub fn add(&mut self, field: Field, amount: i32) -> Result<()> {
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_add(self.0.as_ptr(), field, amount, status) })
    }

    /// Unsets the instant and every field.
    pub fn clear(&mut self) {
        // SAFETY: the calendar is open.
        unsafe { ucal_clear(self.0.as_ptr()) }
    }

    pub fn limit(&mut self, field: Field, limit: Limit) -> Result<i32> {
        // SAFETY: the calendar is open.
        call(|status| unsafe { ucal_getLimit(self.0.as_ptr(), field, limit, status) })
    }

    /// Sunday is 1 and Saturday is 7.
    pub fn first_day_of_week(&self) -> i32 {
        // SAFETY: the calendar is open.
        unsafe { ucal_getAttribute(self.0.as_ptr(), UCAL_FIRST_DAY_OF_WEEK) }
    }

    /// Whether `day` (Sunday is 1 and Saturday is 7) begins inside the weekend.
    pub fn starts_in_weekend(&self, day: i32) -> Result<bool> {
        // SAFETY: the calendar is open, and ICU checks the range of `day`.
        let kind = call(|status| unsafe { ucal_getDayOfWeekType(self.0.as_ptr(), day, status) })?;
        Ok(!matches!(kind, UCAL_WEEKDAY | UCAL_WEEKEND_ONSET))
    }

    /// At the instant that the calendar is set to.
    pub fn zone_offset(&mut self) -> Result<ZoneOffset> {
        Ok(ZoneOffset {
            standard: self.get(Field::ZoneOffset)?,
            daylight: self.get(Field::DaylightOffset)?,
        })
    }

    /// Takes what `set_millis` was given for the reading of a clock in the zone.
    pub fn zone_offset_from_local(&mut self, side: Side) -> Result<ZoneOffset> {
        let mut offset = ZoneOffset::default();
        // SAFETY: the calendar is open.
        call(|status| unsafe {
            ucal_getTimeZoneOffsetFromLocal(
                self.0.as_ptr(),
                side,
                side,
                &mut offset.standard,
                &mut offset.daylight,
                status,
            )
        })?;
        Ok(offset)
    }

    /// The nearest change of the zone's rule, seen from the instant that the calendar is set to. The sum of the offsets can
    /// be the same on both sides.
    pub fn zone_transition(&mut self, direction: Direction) -> Result<Option<f64>> {
        let mut time = 0.0;
        // SAFETY: the calendar is open.
        let found = call(|status| unsafe {
            ucal_getTimeZoneTransitionDate(self.0.as_ptr(), direction, &mut time, status)
        })?;
        Ok((found != 0).then_some(time))
    }

    /// The long name: `Pacific Standard Time`.
    pub fn zone_name(&self, kind: ZoneName, locale: &CStr) -> Result<Buffer<u16>> {
        let locale = locale_id(locale)?;
        // SAFETY: the calendar is open, `locale` is terminated, and `out` has room for `capacity`.
        filled(|out, capacity, status| unsafe {
            ucal_getTimeZoneDisplayName(self.0.as_ptr(), kind, locale, out, capacity, status)
        })
    }
}

fn systems(locale: &CStr, commonly_used: bool) -> Result<Enumeration> {
    let locale = locale_id(locale)?;
    // SAFETY: both strings are terminated.
    Enumeration::open(|status| unsafe {
        ucal_getKeywordValuesForLocale(c"calendar".as_ptr(), locale, commonly_used.into(), status)
    })
}

/// The values of the `calendar` keyword that are in common use in the region of `locale`, the preferred one first.
pub fn common_systems(locale: &CStr) -> Result<Enumeration> {
    systems(locale, true)
}

/// `common_systems`, then all others.
pub fn all_systems(locale: &CStr) -> Result<Enumeration> {
    systems(locale, false)
}
