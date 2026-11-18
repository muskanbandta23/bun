use core::num::NonZeroU64;

use crate::civil::{civil_from_days, week_day};
use crate::time_value::{MAX_LOCAL_TIME, make_date, make_day, make_time};
use crate::zone::Offset;

/// Where a field is in the bits of `DateFields`.
#[derive(Clone, Copy)]
pub struct BitField {
    pub shift: u32,
    pub width: u32,
}

impl BitField {
    const fn above(self, width: u32) -> BitField {
        BitField {
            shift: self.shift + self.width,
            width,
        }
    }

    #[inline(always)]
    pub const fn mask(self) -> u64 {
        (1 << self.width) - 1
    }
}

/// A time value as a calendar and a clock show it, down to the second. It is one word that is not zero, so that generated
/// code can test a cached one and read a field of it with a shift and a mask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(transparent)]
pub struct DateFields(NonZeroU64);

/// The arguments of the constructor of `Date`, in their order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Component {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
}

/// The offset of a zone as `DateFields::new` takes it.
#[derive(Clone, Copy)]
pub(crate) struct PackedOffset {
    pub(crate) milliseconds: i32,
    bits: u32,
}

impl PackedOffset {
    pub(crate) const fn new(offset: Offset) -> PackedOffset {
        let minutes = (offset.seconds / 60) as u64 & DateFields::OFFSET_MINUTES.mask();
        PackedOffset {
            milliseconds: offset.seconds * 1000,
            bits: (minutes << DateFields::OFFSET_MINUTES.shift) as u32 | offset.is_daylight as u32,
        }
    }
}

impl DateFields {
    pub const IS_DAYLIGHT: BitField = BitField { shift: 0, width: 1 };
    /// Signed.
    pub const OFFSET_MINUTES: BitField = Self::IS_DAYLIGHT.above(13);
    pub const SECOND: BitField = Self::OFFSET_MINUTES.above(6);
    pub const MINUTE: BitField = Self::SECOND.above(6);
    pub const HOUR: BitField = Self::MINUTE.above(5);
    pub const WEEK_DAY: BitField = Self::HOUR.above(3);
    pub const DAY: BitField = Self::WEEK_DAY.above(5);
    pub const MONTH: BitField = Self::DAY.above(4);
    /// Signed, and at the top.
    pub const YEAR: BitField = Self::MONTH.above(21);

    /// `None` unless `time` is within a day of the range of time values.
    #[inline]
    pub fn utc(time: f64) -> Option<DateFields> {
        const UTC: PackedOffset = PackedOffset::new(Offset::UTC);
        (time.abs() <= MAX_LOCAL_TIME).then(|| DateFields::new(time as i64, UTC))
    }

    /// `time` is within a day of the range of time values.
    #[inline]
    pub(crate) fn new(time: i64, offset: PackedOffset) -> DateFields {
        const DAYS_BEFORE_EPOCH: u64 = 100_000_002;
        const MS_PER_DAY: u64 = 86_400_000;

        let local_time = time + i64::from(offset.milliseconds);
        let since_start = (local_time as u64).wrapping_add(DAYS_BEFORE_EPOCH * MS_PER_DAY);
        let days_since_start = since_start / MS_PER_DAY;
        let in_day = (since_start % MS_PER_DAY) as u32;
        let days = days_since_start.wrapping_sub(DAYS_BEFORE_EPOCH) as i32;
        let date = civil_from_days(days);
        // hour * 4096 + minute * 64 + second, from the seconds, minutes and hours since midnight.
        let clock = in_day / 1000 + in_day / 60_000 * 4 + in_day / 3_600_000 * 256;

        let bits = (date.year as u64) << Self::YEAR.shift
            | u64::from(date.month - 1) << Self::MONTH.shift
            | u64::from(date.day) << Self::DAY.shift
            | u64::from(week_day(days)) << Self::WEEK_DAY.shift
            | u64::from(clock) << Self::SECOND.shift
            | u64::from(offset.bits);
        DateFields(NonZeroU64::new(bits).unwrap_or(NonZeroU64::MIN))
    }

    #[inline(always)]
    pub const fn to_bits(self) -> u64 {
        self.0.get()
    }

    #[inline(always)]
    const fn unsigned(self, field: BitField) -> u8 {
        (self.to_bits() >> field.shift & field.mask()) as u8
    }

    #[inline(always)]
    const fn signed(self, field: BitField) -> i32 {
        ((self.to_bits() << (64 - field.shift - field.width)) as i64 >> (64 - field.width)) as i32
    }

    #[inline]
    pub const fn year(self) -> i32 {
        self.signed(Self::YEAR)
    }

    /// From zero.
    #[inline]
    pub const fn month(self) -> u8 {
        self.unsigned(Self::MONTH)
    }

    /// From one.
    #[inline]
    pub const fn day(self) -> u8 {
        self.unsigned(Self::DAY)
    }

    /// Sunday is 0.
    #[inline]
    pub const fn week_day(self) -> u8 {
        self.unsigned(Self::WEEK_DAY)
    }

    #[inline]
    pub const fn hour(self) -> u8 {
        self.unsigned(Self::HOUR)
    }

    #[inline]
    pub const fn minute(self) -> u8 {
        self.unsigned(Self::MINUTE)
    }

    #[inline]
    pub const fn second(self) -> u8 {
        self.unsigned(Self::SECOND)
    }

    /// What the clock is ahead of UTC, rounded towards zero.
    #[inline]
    pub const fn offset_minutes(self) -> i32 {
        self.signed(Self::OFFSET_MINUTES)
    }

    #[inline]
    pub const fn is_daylight(self) -> bool {
        self.unsigned(Self::IS_DAYLIGHT) != 0
    }

    /// What a setter of `Date.prototype` passes to UTC or TimeClip: the time on the same clock that has `values` for the
    /// components from `first` on.
    pub fn with_components(self, millisecond: u16, first: Component, values: &[f64]) -> f64 {
        let mut components = [
            self.year().into(),
            self.month().into(),
            self.day().into(),
            self.hour().into(),
            self.minute().into(),
            self.second().into(),
            millisecond.into(),
        ];
        for (component, &value) in components[first as usize..].iter_mut().zip(values) {
            *component = value;
        }
        let [year, month, day, hour, minute, second, millisecond] = components;
        make_date(
            make_day(year, month, day),
            make_time(hour, minute, second, millisecond),
        )
    }
}

const _: () = assert!(DateFields::YEAR.shift + DateFields::YEAR.width == 64);
