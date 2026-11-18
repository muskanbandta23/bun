//! The time zone of the process.

use core::sync::atomic::{AtomicU64, Ordering};

use bun_engine_text::Text;
use bun_icu::{Buffer, time_zone};

use super::id::{TimeZoneId, names_utc, widen};
use super::table::Table;

const LOW_BITS: u32 = 16;
const NO_OVERRIDE: u16 = u16::MAX;

/// The generation, and below it the index in the table of the name that was set.
static OVERRIDE: AtomicU64 = AtomicU64::new(1 << LOW_BITS | NO_OVERRIDE as u64);
/// A generation, and below it the `TimeZoneId` that it has.
static RESOLVED: AtomicU64 = AtomicU64::new(0);

/// Changes whenever the result of `system_time_zone` can have changed. It is never zero.
#[inline]
pub fn time_zone_generation() -> u64 {
    OVERRIDE.load(Ordering::Relaxed) >> LOW_BITS
}

/// Puts the process in another zone, or back in that of the host if `name` is empty. Returns whether ICU knows the name, for
/// which case matters. One that is not available to scripts, such as the offset `GMT+5`, stands for UTC.
pub fn set_time_zone_override(name: Text<'_>) -> bool {
    let index = if name.is_empty() {
        NO_OVERRIDE
    } else if let Some(index) = index_of_known_name(name) {
        index
    } else {
        return false;
    };
    OVERRIDE.update(Ordering::Relaxed, Ordering::Relaxed, |state| {
        ((state >> LOW_BITS) + 1) << LOW_BITS | u64::from(index)
    });
    true
}

fn index_of_known_name(name: Text<'_>) -> Option<u16> {
    const LONGEST: usize = 128;
    if names_utc(name) {
        return Some(Table::UTC);
    }
    let table = Table::get();
    if let Some(index) = table.find(name)
        && name.eq_ascii(table.name(index))
    {
        return Some(index);
    }
    if name.len() > LONGEST || name.code_units().any(|unit| unit == 0) {
        return None;
    }
    let canonical = time_zone::canonical_id(&name.code_units().collect::<Buffer<u16>>()).ok()?;
    Some(table.find(Text::Utf16(&canonical)).unwrap_or(Table::UTC))
}

/// SystemTimeZoneIdentifier: UTC if the zone has no name that is available.
pub fn system_time_zone() -> TimeZoneId {
    let state = OVERRIDE.load(Ordering::Relaxed);
    let resolved = RESOLVED.load(Ordering::Relaxed);
    if resolved >> LOW_BITS == state >> LOW_BITS {
        return TimeZoneId(resolved as u16);
    }
    let zone = match state as u16 {
        NO_OVERRIDE => time_zone::host()
            .ok()
            .and_then(|name| TimeZoneId::primary_of(&name)),
        Table::UTC => None,
        index => TimeZoneId::primary_of(&widen(Table::get().name(index))),
    }
    .unwrap_or(TimeZoneId::UTC);
    RESOLVED.store(
        state >> LOW_BITS << LOW_BITS | u64::from(zone.0),
        Ordering::Relaxed,
    );
    zone
}
