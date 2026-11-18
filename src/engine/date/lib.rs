//! Calendar arithmetic, time zones and date parsing.

#![forbid(unsafe_code)]

mod ascii_buffer;
mod civil;
mod time_value;
mod zone;

pub use ascii_buffer::AsciiBuffer;
pub use civil::{
    CivilDate, civil_from_days, day_of_year, days_from_civil, days_in_month, days_in_year,
    is_leap_year, week_day,
};
pub use time_value::{
    MAX_TIME_VALUE, MS_PER_DAY, MS_PER_HOUR, MS_PER_MINUTE, MS_PER_SECOND, current_time, make_date,
    make_day, make_full_year, make_time, time_clip,
};
pub use zone::{
    Identifier, LocalOffsets, TimeZone, TimeZoneId, ZoneRules, set_time_zone_override,
    system_time_zone, time_zone_generation,
};
