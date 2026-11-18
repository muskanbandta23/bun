use core::cell::{RefCell, RefMut};
use core::ffi::CStr;

use bun_engine_date::TimeZone;
use bun_icu::calendar::Calendar;
use bun_icu::date_format::DateFormat;
use bun_icu::date_pattern::Pattern;

/// The objects of ICU that all formats of a locale share.
pub(super) struct Writer {
    /// With the pattern that it is set to.
    format: RefCell<(DateFormat, Pattern)>,
    utc_calendar: RefCell<Calendar>,
}

impl Writer {
    pub(super) fn open(locale: &CStr) -> bun_icu::Result<Writer> {
        /// Earlier than any date of Temporal.
        const GREGORIAN_CHANGE: f64 = -((1u64 << 53) as f64);

        let mut calendar = Calendar::open(&b"GMT".map(u16::from), locale)?;
        match calendar.set_gregorian_change(GREGORIAN_CHANGE) {
            Ok(()) | Err(bun_icu::Error::UNSUPPORTED) => {}
            Err(error) => return Err(error),
        }
        Ok(Writer {
            format: (DateFormat::open(locale, &[])?, Pattern::new()).into(),
            utc_calendar: calendar.into(),
        })
    }

    pub(super) fn format_for(&self, pattern: &[u16]) -> bun_icu::Result<RefMut<'_, DateFormat>> {
        let (mut format, mut current) =
            RefMut::map_split(self.format.borrow_mut(), |(format, current)| {
                (format, current)
            });
        if **current != *pattern {
            format.set_pattern(pattern)?;
            *current = pattern.into();
        }
        Ok(format)
    }

    pub(super) fn utc_calendar(&self) -> &RefCell<Calendar> {
        &self.utc_calendar
    }

    pub(super) fn new_calendar(&self, time_zone: TimeZone) -> bun_icu::Result<Calendar> {
        let mut calendar = self.utc_calendar.borrow().try_clone()?;
        calendar.set_time_zone(&time_zone.icu_id())?;
        Ok(calendar)
    }
}
