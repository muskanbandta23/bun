//! `Intl.DateTimeFormat`, and the `toLocaleString` methods of `Date` and `Temporal`.

mod cache;
mod components;
mod era;
mod layout;
mod options;
pub(crate) mod pattern;
mod range;
mod selection;
mod text;
mod value;
mod writer;

use core::cell::{OnceCell, RefCell};
use std::rc::Rc;

use bun_engine_date::TimeZone;
use bun_engine_temporal::{CalendarId, ZonedDateTime};
use bun_engine_text::{StringRef, Text};
use bun_icu::CText;
use bun_icu::calendar::Calendar;
use bun_icu::date_format::DateFormat;
use bun_icu::date_interval_format::DateIntervalFormat;
use bun_icu::date_pattern::Pattern;
use bun_icu::formatted_value::FormattedValue;
use smallvec::SmallVec;

pub(crate) use cache::DateCache;
pub use components::{Component, ComponentStyle, ComponentValue, Components};
pub use options::{
    FormatMatcher, Options, Purpose, Style, TIME_ZONE_OPTION_NOT_ACCEPTED, parse_time_zone,
};
pub use value::DateTimeValue;

use crate::error::IcuResult;
use crate::language_tag::Key;
use crate::locale_resolver::{LocaleResolver, ResolvedLocale};
use crate::{
    AvailableLocales, Cache, Error, HourCycle, LanguageTag, Named, NumberingSystem, Parts, Result,
    Source, calendar,
};
use era::EarlyEra;
use layout::{FORMAT_FAILURE, Layout};
use range::RANGE_FAILURE;
use selection::{Clock, Patterns, Selection};
use value::{Moment, TemporalType};
use writer::Writer;

const INITIALIZE_FAILURE: &str = "failed to initialize DateTimeFormat";

/// A clone shares everything, and so do formats that were made from the same arguments.
#[derive(Clone)]
pub struct DateTimeFormat(Rc<State>);

struct State {
    locale: DateLocale,
    time_zone: TimeZone,
    clock: Clock,
    /// As in the options.
    components: Components,
    date_style: Option<Style>,
    time_style: Option<Style>,
    /// For time values.
    layout: Layout,
    temporal_layouts: [OnceCell<TemporalLayout>; TemporalType::COUNT],
    writer: OnceCell<Rc<Writer>>,
    zoned_calendar: OnceCell<RefCell<Calendar>>,
}

enum TemporalLayout {
    OfTimeValues,
    Own(Box<Layout>),
    Inapplicable,
}

struct DateLocale {
    resolved: ResolvedLocale,
    /// With the calendar and the numbering system that were asked for.
    icu_locale: CText,
    /// Set to the default of the locale when it is first needed, unless one was asked for.
    calendar: OnceCell<CalendarId>,
    /// Likewise.
    numbering_system: OnceCell<NumberingSystem>,
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

impl DateLocale {
    fn resolve(
        cache: &Cache,
        requested: &[LanguageTag],
        options: &Options,
    ) -> Result<(DateLocale, Clock)> {
        let mut resolver = LocaleResolver::new(cache, AvailableLocales::general(), requested);
        let calendar = calendar::resolve(&mut resolver, options.calendar.as_ref());
        let clock = match options.hour12 {
            Some(hour12) => Clock::Hour12(hour12),
            None => resolver
                .keyword(
                    Key::HOUR_CYCLE,
                    options.hour_cycle.map(|it| it.as_str().as_bytes()),
                    |name| HourCycle::from_name(Text::Latin1(name)),
                )
                .map_or(Clock::OfLocale, Clock::Cycle),
        };
        let numbering_system =
            NumberingSystem::resolve(&mut resolver, options.numbering_system.as_ref());
        let resolved = resolver.finish()?;
        let locale = DateLocale {
            icu_locale: icu_locale(&resolved, calendar, None, numbering_system.as_ref())?,
            resolved,
            calendar: calendar.map_or_else(OnceCell::new, OnceCell::from),
            numbering_system: numbering_system.map_or_else(OnceCell::new, OnceCell::from),
        };
        Ok((locale, clock))
    }

    fn calendar(&self) -> Result<CalendarId> {
        (self.calendar)
            .get_or_try_init(|| calendar::default_for(&self.icu_locale))
            .copied()
    }

    fn numbering_system(&self) -> Result<NumberingSystem> {
        (self.numbering_system)
            .get_or_try_init(|| NumberingSystem::default_for(&self.icu_locale))
            .copied()
    }

    fn icu_locale_with(&self, hour_cycle: Option<HourCycle>) -> Result<CText> {
        let calendar = self.calendar.get().copied();
        icu_locale(
            &self.resolved,
            calendar,
            hour_cycle,
            self.numbering_system.get(),
        )
    }

    fn layout(&self, pattern: Pattern) -> Result<Layout> {
        let shows_era = pattern::fields(&pattern).any(|field| field.letter == b'G');
        Ok(Layout {
            early_era: if shows_era {
                EarlyEra::of(self.calendar()?)
            } else {
                None
            },
            pattern,
            range_format: OnceCell::new(),
        })
    }
}

fn icu_locale(
    locale: &ResolvedLocale,
    calendar: Option<CalendarId>,
    hour_cycle: Option<HourCycle>,
    numbering_system: Option<&NumberingSystem>,
) -> Result<CText> {
    let keywords: SmallVec<[_; 3]> = [
        calendar.map(|it| (Key::CALENDAR, it.identifier().as_bytes())),
        hour_cycle.map(|it| (Key::HOUR_CYCLE, it.as_str().as_bytes())),
        numbering_system.map(|it| (Key::NUMBERING_SYSTEM, it.as_bytes())),
    ]
    .into_iter()
    .flatten()
    .collect();
    locale.icu_locale(&keywords)
}

fn check_styles(options: &Options, purpose: Purpose) -> Result<()> {
    if options.components != Components::default() {
        return Err(Error::Type(
            "dateStyle and timeStyle may not be used with other DateTimeFormat options",
        ));
    }
    if purpose == Purpose::Date && options.time_style.is_some() {
        return Err(Error::Type(
            "timeStyle is specified while formatting date is requested",
        ));
    }
    if purpose == Purpose::Time && options.date_style.is_some() {
        return Err(Error::Type(
            "dateStyle is specified while formatting time is requested",
        ));
    }
    Ok(())
}

impl State {
    fn new(
        cache: &Cache,
        requested: &[LanguageTag],
        options: &Options,
        purpose: Purpose,
    ) -> Result<State> {
        let (locale, clock) = DateLocale::resolve(cache, requested, options)?;
        let patterns = Patterns {
            cache: &cache.dates,
            locale: &locale.icu_locale,
            clock,
        };
        let pattern = match (options.date_style, options.time_style) {
            (None, None) => patterns.for_components(&purpose.add_defaults(options.components)),
            (date_style, time_style) => {
                check_styles(options, purpose)?;
                patterns.for_styles(date_style, time_style)
            }
        };
        Ok(State {
            layout: locale.layout(pattern.or_type_error(INITIALIZE_FAILURE)?)?,
            locale,
            time_zone: options.time_zone,
            clock,
            components: options.components,
            date_style: options.date_style,
            time_style: options.time_style,
            temporal_layouts: Default::default(),
            writer: OnceCell::new(),
            zoned_calendar: OnceCell::new(),
        })
    }

    fn has_style(&self) -> bool {
        self.date_style.is_some() || self.time_style.is_some()
    }

    fn make_temporal_layout(
        &self,
        cache: &Cache,
        temporal_type: TemporalType,
    ) -> Result<TemporalLayout> {
        let patterns = Patterns {
            cache: &cache.dates,
            locale: &self.locale.icu_locale,
            clock: self.clock,
        };
        let selection = Selection::for_type(temporal_type);
        let pattern = if self.has_style() {
            patterns.adjust_style(&self.layout.pattern, selection)
        } else {
            (selection.apply(self.components))
                .map(|components| patterns.for_components(&components))
                .transpose()
        };
        Ok(match pattern.or_type_error(INITIALIZE_FAILURE)? {
            Some(pattern) if pattern == self.layout.pattern => TemporalLayout::OfTimeValues,
            Some(pattern) => TemporalLayout::Own(Box::new(self.locale.layout(pattern)?)),
            None => TemporalLayout::Inapplicable,
        })
    }

    /// HandleDateTimeValue, but for the check that a number is a time value.
    fn handle(&self, cache: &Cache, value: DateTimeValue) -> Result<(&Layout, Moment)> {
        let moment = value.moment()?;
        if let Some(calendar) = moment.calendar
            && calendar != self.locale.calendar()?
        {
            return Err(Error::Range(
                "Temporal object's calendar does not match DateTimeFormat calendar",
            ));
        }
        let Some(temporal_type) = moment.temporal_type else {
            return Ok((&self.layout, moment));
        };
        let layout = (self.temporal_layouts[temporal_type as usize])
            .get_or_try_init(|| self.make_temporal_layout(cache, temporal_type))?;
        match layout {
            TemporalLayout::OfTimeValues => Ok((&self.layout, moment)),
            TemporalLayout::Own(layout) => Ok((layout, moment)),
            TemporalLayout::Inapplicable => Err(Error::Type(
                "DateTimeFormat has no fields applicable to this Temporal type",
            )),
        }
    }

    fn writer(&self, cache: &Cache) -> Result<&Writer> {
        (self.writer)
            .get_or_try_init(|| cache.dates.writer(&self.locale.icu_locale))
            .map(|writer| &**writer)
            .or_type_error(INITIALIZE_FAILURE)
    }

    fn calendar_for(&self, cache: &Cache, moment: &Moment) -> Result<&RefCell<Calendar>> {
        let writer = self.writer(cache)?;
        if moment.is_plain() {
            return Ok(writer.utc_calendar());
        }
        (self.zoned_calendar)
            .get_or_try_init(|| writer.new_calendar(self.time_zone).map(RefCell::new))
            .or_type_error(INITIALIZE_FAILURE)
    }

    fn write<T>(
        &self,
        cache: &Cache,
        layout: &Layout,
        moment: &Moment,
        write: impl FnOnce(&Layout, &DateFormat, &mut Calendar) -> Result<T>,
    ) -> Result<T> {
        let mut calendar = self.calendar_for(cache, moment)?.borrow_mut();
        (calendar.set_millis(moment.time)).or_type_error(FORMAT_FAILURE)?;
        let format = self.writer(cache)?.format_for(&layout.pattern);
        write(
            layout,
            &*format.or_type_error(FORMAT_FAILURE)?,
            &mut calendar,
        )
    }

    fn range_format<'a>(&self, layout: &'a Layout) -> Result<&'a DateIntervalFormat> {
        (layout.range_format).get_or_try_init(|| {
            // ICU takes from the skeleton only whether the clock has twelve hours.
            let locale = (self.locale).icu_locale_with(pattern::hour_cycle(&layout.pattern))?;
            pattern::skeleton(&layout.pattern)
                .and_then(|skeleton| DateIntervalFormat::open(&locale, &skeleton))
                .or_type_error("failed to initialize DateIntervalFormat")
        })
    }

    /// PartitionDateTimeRangePattern
    fn write_range<T>(
        &self,
        cache: &Cache,
        [start, end]: [DateTimeValue; 2],
        write_range: impl FnOnce(FormattedValue<'_>) -> Result<T>,
        write_single: impl FnOnce(&Layout, &DateFormat, &mut Calendar) -> Result<T>,
    ) -> Result<T> {
        if !start.is_same_type(&end) {
            return Err(Error::Type(
                "formatRange requires both arguments to be the same Temporal type",
            ));
        }
        let (layout, start) = self.handle(cache, start)?;
        let (_, end) = self.handle(cache, end)?;
        if start.time.is_nan() || end.time.is_nan() {
            return Err(Error::Range("Passed date is out of range"));
        }
        let format = self.range_format(layout)?;
        let range = {
            let mut start_calendar = self.calendar_for(cache, &start)?.borrow_mut();
            (start_calendar.try_clone()).and_then(|mut end_calendar| {
                start_calendar.set_millis(start.time)?;
                end_calendar.set_millis(end.time)?;
                format.format(&mut start_calendar, &mut end_calendar)
            })
        };
        let range = range.or_type_error(RANGE_FAILURE)?;
        let value = range.value().or_type_error(RANGE_FAILURE)?;
        if range::shows_both_ends(value)? {
            write_range(value)
        } else {
            self.write(cache, layout, &start, write_single)
        }
    }
}

impl DateTimeFormat {
    /// CreateDateTimeFormat
    pub fn new(
        cache: &Cache,
        requested: &[LanguageTag],
        options: &Options,
        purpose: Purpose,
    ) -> Result<DateTimeFormat> {
        if let Some(format) = cache.dates.find_format(requested, options, purpose) {
            return Ok(format);
        }
        let format = DateTimeFormat(Rc::new(State::new(cache, requested, options, purpose)?));
        (cache.dates).keep_format(requested, options, purpose, &format);
        Ok(format)
    }

    pub fn format(&self, cache: &Cache, value: DateTimeValue) -> Result<StringRef> {
        let (layout, moment) = self.0.handle(cache, value)?;
        if moment.time.is_nan() {
            return Err(Error::Range(
                "date value is not finite in DateTimeFormat format()",
            ));
        }
        self.0.write(cache, layout, &moment, Layout::string)
    }

    pub fn format_to_parts(&self, cache: &Cache, value: DateTimeValue) -> Result<Parts> {
        let (layout, moment) = self.0.handle(cache, value)?;
        if moment.time.is_nan() {
            return Err(Error::Range(
                "date value is not finite in DateTimeFormat formatToParts()",
            ));
        }
        (self.0).write(cache, layout, &moment, |layout, format, calendar| {
            layout.parts(format, calendar, None)
        })
    }

    pub fn format_range(
        &self,
        cache: &Cache,
        start: DateTimeValue,
        end: DateTimeValue,
    ) -> Result<StringRef> {
        (self.0).write_range(cache, [start, end], range::string, Layout::string)
    }

    pub fn format_range_to_parts(
        &self,
        cache: &Cache,
        start: DateTimeValue,
        end: DateTimeValue,
    ) -> Result<Parts> {
        self.0.write_range(
            cache,
            [start, end],
            range::parts,
            |layout, format, calendar| layout.parts(format, calendar, Some(Source::Shared)),
        )
    }

    /// For a format that was made with `Purpose::ZonedDateTime` and the time zone of `value`.
    pub fn format_zoned_date_time(
        &self,
        cache: &Cache,
        value: &ZonedDateTime,
    ) -> Result<StringRef> {
        let calendar = value.calendar();
        if calendar != CalendarId::Iso8601 && calendar != self.0.locale.calendar()? {
            return Err(Error::Range(
                "ZonedDateTime calendar does not match locale calendar",
            ));
        }
        let time = value.instant().epoch_milliseconds();
        self.format(cache, DateTimeValue::Number(time as f64))
    }

    pub fn resolved_options(&self) -> Result<ResolvedOptions<'_>> {
        let state = &*self.0;
        let hour_cycle = pattern::hour_cycle(&state.layout.pattern);
        Ok(ResolvedOptions {
            locale: state.locale.resolved.tag(),
            calendar: state.locale.calendar()?,
            numbering_system: state.locale.numbering_system()?,
            time_zone: state.time_zone,
            hour_cycle,
            hour12: hour_cycle.map(HourCycle::is_twelve_hour),
            components: if state.has_style() {
                Components::default()
            } else {
                pattern::components(&state.layout.pattern)
            },
            date_style: state.date_style,
            time_style: state.time_style,
        })
    }
}
