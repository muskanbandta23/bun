//! `Intl.DateTimeFormat`, and the `toLocaleString` methods of `Date` and `Temporal`.

mod cache;
mod components;
mod era;
mod formats;
mod layout;
mod locale_data;
mod options;
pub(crate) mod pattern;
mod range;
mod selection;
mod template;
mod text;
mod value;

use core::cell::{OnceCell, RefMut};
use std::rc::Rc;

use bun_engine_date::TimeZone;
use bun_engine_temporal::{CalendarId, ZonedDateTime};
use bun_engine_text::StringRef;
use bun_icu::calendar::Calendar;
use bun_icu::date_format::DateFormat;
use bun_icu::date_interval_format::{DateIntervalFormat, FormattedDateInterval};
use bun_icu::formatted_value::FormattedValue;

pub(crate) use cache::DateCache;
pub use components::{Component, ComponentStyle, ComponentValue, Components};
pub use options::{
    FormatMatcher, Options, Purpose, Style, TIME_ZONE_OPTION_NOT_ACCEPTED, parse_time_zone,
};
pub use value::DateTimeValue;

use crate::error::IcuResult;
use crate::parts::new_string;
use crate::{
    Cache, Error, HourCycle, LanguageTag, NumberingSystem, Part, PartKind, Parts, Result, Source,
};
use formats::Formats;
use layout::{FORMAT_FAILURE, Layout};
use locale_data::Calendars;
use range::RANGE_FAILURE;
use template::Template;
use text::{DateText, use_plain_spaces};
use value::Moment;

pub struct DateTimeFormat {
    formats: Rc<Formats>,
    time_zone: TimeZone,
    zoned_calendars: OnceCell<Rc<Calendars>>,
}

pub struct ResolvedOptions<'a> {
    pub locale: &'a LanguageTag,
    pub calendar: CalendarId,
    pub numbering_system: NumberingSystem,
    pub time_zone: TimeZone,
    pub hour_cycle: Option<HourCycle>,
    pub hour12: Option<bool>,
    pub components: Components,
    pub date_style: Option<Style>,
    pub time_style: Option<Style>,
}

impl DateTimeFormat {
    /// CreateDateTimeFormat. `time_zone` is that of the system if the option is absent.
    pub fn new(
        cache: &Cache,
        requested: &[LanguageTag],
        options: &Options,
        time_zone: TimeZone,
        purpose: Purpose,
    ) -> Result<DateTimeFormat> {
        let formats = match cache.dates.find_formats(requested, options, purpose) {
            Some(formats) => formats,
            None => {
                let formats = Rc::new(Formats::new(cache, requested, options, purpose)?);
                (cache.dates).keep_formats(requested, options, purpose, &formats);
                formats
            }
        };
        Ok(DateTimeFormat {
            formats,
            time_zone,
            zoned_calendars: OnceCell::new(),
        })
    }

    pub fn format(&self, cache: &Cache, value: DateTimeValue) -> Result<StringRef> {
        let (layout, moment) = self.formats.handle(cache, value)?;
        if moment.time.is_nan() {
            return Err(Error::Range(
                "date value is not finite in DateTimeFormat format()",
            ));
        }
        self.string(layout, &moment)
    }

    pub fn format_to_parts(&self, cache: &Cache, value: DateTimeValue) -> Result<Parts> {
        let (layout, moment) = self.formats.handle(cache, value)?;
        if moment.time.is_nan() {
            return Err(Error::Range(
                "date value is not finite in DateTimeFormat formatToParts()",
            ));
        }
        self.parts(layout, &moment, None)
    }

    pub fn format_range(
        &self,
        cache: &Cache,
        start: DateTimeValue,
        end: DateTimeValue,
    ) -> Result<StringRef> {
        self.write_range(cache, [start, end], range::string, DateTimeFormat::string)
    }

    pub fn format_range_to_parts(
        &self,
        cache: &Cache,
        start: DateTimeValue,
        end: DateTimeValue,
    ) -> Result<Parts> {
        self.write_range(cache, [start, end], range::parts, |this, layout, moment| {
            this.parts(layout, moment, Some(Source::Shared))
        })
    }

    /// For a format that was made with `Purpose::ZonedDateTime` and the time zone of `value`.
    pub fn format_zoned_date_time(
        &self,
        cache: &Cache,
        value: &ZonedDateTime,
    ) -> Result<StringRef> {
        let calendar = value.calendar();
        if calendar != CalendarId::Iso8601 && calendar != self.formats.data.calendar()? {
            return Err(Error::Range(
                "ZonedDateTime calendar does not match locale calendar",
            ));
        }
        let time = value.instant().epoch_milliseconds();
        self.format(cache, DateTimeValue::Number(time as f64))
    }

    pub fn resolved_options(&self) -> Result<ResolvedOptions<'_>> {
        let formats = &*self.formats;
        let pattern = &formats.of_time_values.pattern;
        let hour_cycle = pattern::hour_cycle(pattern);
        Ok(ResolvedOptions {
            locale: formats.locale.tag(),
            calendar: formats.data.calendar()?,
            numbering_system: formats.data.numbering_system()?,
            time_zone: self.time_zone,
            hour_cycle,
            hour12: hour_cycle.map(HourCycle::is_twelve_hour),
            components: if formats.has_style() {
                Components::default()
            } else {
                pattern::components(pattern)
            },
            date_style: formats.date_style,
            time_style: formats.time_style,
        })
    }

    /// In milliseconds.
    fn local_time(&self, moment: &Moment) -> i64 {
        let time = moment.time as i64;
        let offset_seconds = match self.time_zone {
            _ if moment.is_plain() => 0,
            TimeZone::Named(id) => id.rules().offset_at(time.div_euclid(1000)),
            TimeZone::Offset { minutes } => i32::from(minutes) * 60,
        };
        time + i64::from(offset_seconds) * 1000
    }

    fn calendars(&self, moment: &Moment) -> bun_icu::Result<&Calendars> {
        let data = &self.formats.data;
        if moment.is_plain() {
            return data.utc_calendars();
        }
        (self.zoned_calendars)
            .get_or_try_init(|| data.zoned_calendars(self.time_zone))
            .map(|calendars| &**calendars)
    }

    fn prepare(
        &self,
        pattern: &[u16],
        moment: &Moment,
    ) -> bun_icu::Result<(RefMut<'_, DateFormat>, RefMut<'_, Calendar>)> {
        let mut calendar = self.calendars(moment)?.first.borrow_mut();
        calendar.set_millis(moment.time)?;
        Ok((self.formats.data.format(pattern)?, calendar))
    }

    fn fill(
        &self,
        template: &Template,
        moment: &Moment,
        text: &mut DateText,
        each_part: impl FnMut(PartKind, &[u16]) -> Result<()>,
    ) -> Result<()> {
        let mut zone_name = DateText::new();
        if let Some(field) = template.time_zone_field() {
            self.prepare(field, moment)
                .and_then(|(format, mut calendar)| format.format(&mut calendar, &mut zone_name))
                .or_type_error(FORMAT_FAILURE)?;
            use_plain_spaces(&mut zone_name);
        }
        template.write(self.local_time(moment), &zone_name, text, each_part)
    }

    fn string(&self, layout: &Layout, moment: &Moment) -> Result<StringRef> {
        let Some(template) = layout.template(&self.formats.data)? else {
            let (format, mut calendar) =
                (self.prepare(&layout.pattern, moment)).or_type_error(FORMAT_FAILURE)?;
            return layout.string(&format, &mut calendar);
        };
        let mut text = DateText::new();
        self.fill(template, moment, &mut text, |_, _| Ok(()))?;
        new_string(&text)
    }

    fn parts(&self, layout: &Layout, moment: &Moment, source: Option<Source>) -> Result<Parts> {
        let Some(template) = layout.template(&self.formats.data)? else {
            let (format, mut calendar) =
                (self.prepare(&layout.pattern, moment)).or_type_error(FORMAT_FAILURE)?;
            return layout.parts(&format, &mut calendar, source);
        };
        let (mut text, mut parts) = (DateText::new(), Parts::default());
        self.fill(template, moment, &mut text, |kind, value| {
            let unit = None;
            parts.push(Part { kind, unit, source }, value)
        })?;
        Ok(parts)
    }

    fn range(
        &self,
        format: &DateIntervalFormat,
        start: &Moment,
        end: &Moment,
    ) -> Result<FormattedDateInterval> {
        let range = self.calendars(start).and_then(|calendars| {
            let mut second = calendars.second()?.borrow_mut();
            let mut first = calendars.first.borrow_mut();
            first.set_millis(start.time)?;
            second.set_millis(end.time)?;
            format.format(&mut first, &mut second)
        });
        range.or_type_error(RANGE_FAILURE)
    }

    /// PartitionDateTimeRangePattern
    fn write_range<T>(
        &self,
        cache: &Cache,
        [start, end]: [DateTimeValue; 2],
        write_range: impl FnOnce(FormattedValue<'_>) -> Result<T>,
        write_single: impl FnOnce(&DateTimeFormat, &Layout, &Moment) -> Result<T>,
    ) -> Result<T> {
        if !start.is_same_type(&end) {
            return Err(Error::Type(
                "formatRange requires both arguments to be the same Temporal type",
            ));
        }
        let (layout, start) = self.formats.handle(cache, start)?;
        let (_, end) = self.formats.handle(cache, end)?;
        if start.time.is_nan() || end.time.is_nan() {
            return Err(Error::Range("Passed date is out of range"));
        }
        let range = self.range(self.formats.range_format(cache, layout)?, &start, &end)?;
        let value = range.value().or_type_error(RANGE_FAILURE)?;
        if range::shows_both_ends(value)? {
            write_range(value)
        } else {
            write_single(self, layout, &start)
        }
    }
}
