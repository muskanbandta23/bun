use core::cell::{Cell, RefCell};

use bun_engine_text::{StringRef, Text};
use bun_icu::calendar::{Calendar, ZoneName};
use bun_icu::host_language;

use crate::ascii_buffer::AsciiBuffer;
use crate::fields::{DateFields, PackedOffset};
use crate::parse::{Parsed, parse_date};
use crate::time_value::{MAX_LOCAL_TIME, time_clip};
use crate::zone::{
    Offset, Side, Span, TimeZone, TimeZoneId, ZoneRules, system_time_zone, time_zone_generation,
};

/// A span in milliseconds.
#[derive(Clone, Copy)]
struct CachedSpan {
    start: i64,
    length: u64,
    offset: PackedOffset,
}

impl CachedSpan {
    const EMPTY: CachedSpan = CachedSpan {
        start: 0,
        length: 0,
        offset: PackedOffset::new(Offset::UTC),
    };

    #[inline]
    fn new(span: Span) -> CachedSpan {
        const LIMIT: i64 = MAX_LOCAL_TIME as i64 / 1000 + 1;
        let (start, end) = (span.start.max(-LIMIT), span.end.min(LIMIT));
        CachedSpan {
            start: start * 1000,
            length: (end - start) as u64 * 1000,
            offset: PackedOffset::new(span.offset),
        }
    }

    #[inline(always)]
    fn contains(&self, time: i64) -> bool {
        (time.wrapping_sub(self.start) as u64) < self.length
    }
}

/// Local time as one thread sees it. It stays in the zone that the process was in when it first needed one, until `reset`.
pub struct DateCache {
    generation: Cell<u64>,
    zone: Cell<Option<(TimeZoneId, &'static ZoneRules)>>,
    utc_span: Cell<CachedSpan>,
    local_span: Cell<CachedSpan>,
    has_made_local_fields: Cell<bool>,
    /// For standard time and for daylight saving time, with what goes around them.
    zone_names: RefCell<Option<[StringRef; 2]>>,
}

impl Default for DateCache {
    fn default() -> DateCache {
        DateCache {
            generation: Cell::new(time_zone_generation()),
            zone: Cell::new(None),
            utc_span: Cell::new(CachedSpan::EMPTY),
            local_span: Cell::new(CachedSpan::EMPTY),
            has_made_local_fields: Cell::new(false),
            zone_names: RefCell::new(None),
        }
    }
}

impl DateCache {
    /// Whether the zone of the process can have changed since the last `reset`.
    #[inline]
    pub fn is_stale(&self) -> bool {
        self.generation.get() != time_zone_generation()
    }

    /// Returns whether `local_fields` has been called since the last time, whose results are now out of date.
    pub fn reset(&self) -> bool {
        self.generation.set(time_zone_generation());
        self.zone.set(None);
        self.utc_span.set(CachedSpan::EMPTY);
        self.local_span.set(CachedSpan::EMPTY);
        self.zone_names.take();
        self.has_made_local_fields.take()
    }

    pub fn time_zone(&self) -> TimeZoneId {
        self.zone().0
    }

    #[inline]
    fn zone(&self) -> (TimeZoneId, &'static ZoneRules) {
        self.zone.get().unwrap_or_else(|| {
            let id = system_time_zone();
            let zone = (id, id.rules());
            self.zone.set(Some(zone));
            zone
        })
    }

    /// The fields of LocalTime(`time`). `None` unless `time` is within a day of the range of time values.
    #[inline]
    pub fn local_fields(&self, time: f64) -> Option<DateFields> {
        if time.abs() <= MAX_LOCAL_TIME {
            let time = time as i64;
            let mut span = self.utc_span.get();
            if !span.contains(time) {
                span = self.find_utc_span(time);
            }
            return Some(DateFields::new(time, span.offset));
        }
        None
    }

    #[inline(never)]
    fn find_utc_span(&self, time: i64) -> CachedSpan {
        self.has_made_local_fields.set(true);
        let span = CachedSpan::new(self.zone().1.span_at(time.div_euclid(1000)));
        self.utc_span.set(span);
        span
    }

    /// UTC(`local_time`). What is too far out to become a time value is returned as it is.
    #[inline]
    pub fn utc(&self, local_time: f64) -> f64 {
        if local_time.abs() <= MAX_LOCAL_TIME {
            return local_time - f64::from(self.offset_at_local(local_time as i64));
        }
        local_time
    }

    #[inline]
    fn offset_at_local(&self, local_time: i64) -> i32 {
        let mut span = self.local_span.get();
        if !span.contains(local_time) {
            span = self.find_local_span(local_time);
        }
        span.offset.milliseconds
    }

    #[inline(never)]
    fn find_local_span(&self, local_time: i64) -> CachedSpan {
        let rules = self.zone().1;
        let span = CachedSpan::new(rules.span_at_local(local_time.div_euclid(1000), Side::Before));
        self.local_span.set(span);
        span
    }

    /// `Date.parse`.
    pub fn parse(&self, text: Text<'_>) -> f64 {
        match parse_date(text) {
            Some(Parsed::Utc(time)) => time_clip(time as f64),
            Some(Parsed::Local(time)) => time_clip(self.utc(time as f64)),
            None => f64::NAN,
        }
    }

    /// `Tue Feb 01 2022 13:05:09 GMT+0100 (Central European Standard Time)`
    pub fn date_time_string(&self, fields: DateFields) -> StringRef {
        let mut text = AsciiBuffer::<36>::new();
        text.push_date(fields);
        text.push(b" ");
        text.push_time_and_offset(fields);
        self.with_zone_name(text.as_bytes(), fields)
    }

    /// `13:05:09 GMT+0100 (Central European Standard Time)`
    pub fn time_string(&self, fields: DateFields) -> StringRef {
        let mut text = AsciiBuffer::<17>::new();
        text.push_time_and_offset(fields);
        self.with_zone_name(text.as_bytes(), fields)
    }

    fn with_zone_name(&self, ascii: &[u8], fields: DateFields) -> StringRef {
        let mut names = self.zone_names.borrow_mut();
        let names = names.get_or_insert_with(|| parenthesized_names(self.time_zone()));
        let name = names[usize::from(fields.is_daylight())].text();
        StringRef::try_concat(&[Text::Latin1(ascii), name])
            .unwrap_or_else(|_| StringRef::from_latin1(ascii))
    }
}

#[cold]
fn parenthesized_names(zone: TimeZoneId) -> [StringRef; 2] {
    let calendar = Calendar::open(&TimeZone::Named(zone).icu_id(), c"").ok();
    [ZoneName::Standard, ZoneName::Daylight]
        .map(|kind| {
            let name = calendar.as_ref()?.zone_name(kind, host_language()).ok()?;
            let name = StringRef::try_narrowed(Text::Utf16(&name)).ok()?;
            let parts = [Text::from_ascii(" ("), name.text(), Text::from_ascii(")")];
            StringRef::try_concat(&parts)
                .ok()
                .filter(|_| !name.is_empty())
        })
        .map(Option::unwrap_or_default)
}
